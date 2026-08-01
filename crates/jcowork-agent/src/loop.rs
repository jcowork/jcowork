//! Agent Loop - core orchestrator for LLM calls, tool dispatch, and streaming.

use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

use jcowork_llm::provider::{ChatMessage, LlmProvider, StreamChunk, ToolCall, Usage};
use jcowork_memory::MemoryManager;
use jcowork_skills::SkillManager;
use jcowork_tools::base::ToolContext;
use jcowork_tools::registry::ToolRegistry;

use crate::context::{Compressor, ContextEngine};
use crate::prompt::PromptBuilder;

/// Message sent from WebSocket to AgentLoop.
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub session_id: String,
    pub content: String,
}

/// Message sent from AgentLoop back to the client.
#[derive(Debug, Clone)]
pub enum AgentOutput {
    /// A text chunk from the LLM's response.
    TextDelta(String),
    /// A tool is being called.
    ToolCallStart { name: String, call_id: String },
    /// A tool has completed.
    ToolCallEnd { name: String, result: String },
    /// The agent's full turn is complete.
    Done { usage: Usage },
    /// An error occurred.
    Error(String),
}

/// Configuration for an agent instance.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub user_id: String,
    pub workspace_root: String,
    pub model: String,
    pub max_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            workspace_root: String::new(),
            model: "openai:gpt-4o".to_string(),
            max_turns: 20,
        }
    }
}

/// The core agent loop.
///
/// Orchestrates the conversation with the LLM:
/// 1. Build system prompt (identity + memory + skills + context)
/// 2. Send messages + tool schemas to LLM
/// 3. Stream back response text and tool calls
/// 4. Dispatch tool calls via ToolRegistry
/// 5. Add tool results to message history
/// 6. Repeat until LLM stops making tool calls
/// 7. Check if context compression is needed
///
/// Agent loop: orchestrates LLM calls, tool dispatch, and conversation history.
pub struct AgentLoop {
    config: AgentConfig,
    messages: Vec<ChatMessage>,
    tool_registry: Arc<ToolRegistry>,
    memory_manager: Arc<MemoryManager>,
    skill_manager: Arc<SkillManager>,
    llm_provider: Arc<dyn LlmProvider>,
    context_engine: Box<dyn ContextEngine>,
}

impl AgentLoop {
    /// Create a new agent loop with all dependencies.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        memory_manager: Arc<MemoryManager>,
        skill_manager: Arc<SkillManager>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let context_engine = Box::new(Compressor::new(llm_provider.context_length()));

        Self {
            config,
            messages: Vec::new(),
            tool_registry,
            memory_manager,
            skill_manager,
            llm_provider,
            context_engine,
        }
    }

    /// Run the agent loop for a single user message.
    ///
    /// Streams output back to the caller via the provided sender.
    /// The loop continues until the LLM stops making tool calls
    /// or max_turns is reached.
    pub async fn run(
        &mut self,
        user_message: &str,
        output_tx: mpsc::Sender<AgentOutput>,
    ) -> Result<()> {
        // Pre-turn: prefetch memory
        let memory_context = self.memory_manager
            .build_system_prompt(&self.config.user_id)
            .await;

        // Build system prompt
        let skill_index = self.skill_manager
            .build_skill_index(&self.config.user_id)
            .await;

        let system_prompt = PromptBuilder::new()
            .memory_context(memory_context)
            .skill_index(skill_index)
            .build();

        // Ensure system prompt is present
        if self.messages.is_empty() {
            self.messages.push(ChatMessage {
                role: "system".to_string(),
                content: system_prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        } else {
            // Update system prompt on first message
            self.messages[0].content = system_prompt;
        }

        // Add user message
        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: user_message.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        // Agent loop: keep calling LLM until it stops making tool calls
        let mut turns = 0;
        loop {
            turns += 1;
            if turns > self.config.max_turns {
                let _ = output_tx.send(AgentOutput::Error(
                    "Max turns reached".to_string(),
                )).await;
                break;
            }

            // Check context compression
            if self.context_engine.should_compress(
                self.estimate_tokens(),
            ) {
                self.messages = self.context_engine
                    .compress(self.messages.clone(), self.estimate_tokens())
                    .await?;
            }

            // Call LLM with streaming
            let tools = self.tool_registry.all_schemas();
            let stream_result = self.llm_provider
                .chat_stream(&self.messages, &tools)
                .await;

            let mut stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    let _ = output_tx.send(AgentOutput::Error(
                        format!("LLM error: {}", e),
                    )).await;
                    break;
                }
            };

            // Process the stream
            let mut assistant_content = String::new();
            let mut reasoning_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool_args: HashMap<String, (String, String, String)> = HashMap::new();

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(StreamChunk::Delta(text)) => {
                        assistant_content.push_str(&text);
                        let _ = output_tx.send(AgentOutput::TextDelta(text)).await;
                    }
                    Ok(StreamChunk::ReasoningDelta(reasoning)) => {
                        reasoning_content.push_str(&reasoning);
                        // Accumulate reasoning for history, don't send to client
                    }
                    Ok(StreamChunk::ToolCallDelta(call_id, func_name, args_delta)) => {
                        let entry = current_tool_args
                            .entry(call_id.clone())
                            .or_insert_with(|| (call_id.clone(), func_name.clone(), String::new()));
                        entry.2.push_str(&args_delta);

                        let _ = output_tx.send(AgentOutput::ToolCallStart {
                            name: func_name.clone(),
                            call_id: call_id.clone(),
                        }).await;
                    }
                    Ok(StreamChunk::Done(usage)) => {
                        self.context_engine.update_from_response(&usage);
                        let _ = output_tx.send(AgentOutput::Done { usage }).await;
                    }
                    Err(e) => {
                        let _ = output_tx.send(AgentOutput::Error(
                            format!("Stream error: {}", e),
                        )).await;
                        break;
                    }
                }
            }

            // Build tool calls from accumulated deltas
            for (_, (call_id, func_name, arguments)) in current_tool_args {
                tool_calls.push(ToolCall {
                    id: call_id,
                    r#type: "function".to_string(),
                    function: jcowork_llm::provider::FunctionCall {
                        name: func_name,
                        arguments,
                    },
                });
            }

            // Add assistant message to history
            let assistant_message = ChatMessage {
                role: "assistant".to_string(),
                content: assistant_content,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls.clone()) },
                tool_call_id: None,
                reasoning_content: if reasoning_content.is_empty() { None } else { Some(reasoning_content) },
            };
            self.messages.push(assistant_message);

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                break;
            }

            // Dispatch tool calls
            let tool_ctx = ToolContext {
                user_id: self.config.user_id.clone(),
                workspace_root: self.config.workspace_root.clone(),
            };

            for tc in &tool_calls {
                let result = self.tool_registry
                    .dispatch_isolated(&tc.function.name, &tc.function.arguments, &tool_ctx)
                    .await;

                let result_str = match result {
                    Ok(r) => r,
                    Err(e) => format!("Error: {}", e),
                };

                let _ = output_tx.send(AgentOutput::ToolCallEnd {
                    name: tc.function.name.clone(),
                    result: result_str.clone(),
                }).await;

                // Add tool result message
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result_str,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    reasoning_content: None,
                });
            }
        }

        // Post-turn: memory nudge (periodically remind agent to persist knowledge)
        Ok(())
    }

    /// Rough token estimate (4 chars per token).
    fn estimate_tokens(&self) -> i32 {
        let total_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
        (total_chars / 4) as i32
    }

    /// Reset the conversation (new session).
    pub fn reset(&mut self) {
        self.messages.clear();
    }

    /// Get the current message history.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }
}

use std::collections::HashMap;
