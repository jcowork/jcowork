#!/usr/bin/env node
/**
 * Test the frontend handleMove logic by simulating what the drag-and-drop does.
 * This verifies the path computation is correct.
 */

// Simulate the handleMove logic from Documents.tsx
function computeMoveParams(fromPath, targetDirPath) {
  const name = fromPath.split('/').pop() || fromPath;
  const to = targetDirPath;
  
  // Guard: prevent moving into self or descendant
  if (to === fromPath || to.startsWith(fromPath + '/')) {
    return { error: `Cannot move "${name}" into itself or its subfolder.` };
  }
  
  // Guard: prevent no-op
  const fromParent = fromPath.includes('/') ? fromPath.substring(0, fromPath.lastIndexOf('/')) : '';
  if (fromParent === to) return { error: 'no-op' };
  
  const toPath = to ? `${to}/${name}` : name;
  if (toPath === fromPath) return { error: 'no-op' };
  
  return { from: fromPath, to: toPath };
}

let passed = 0, failed = 0;

function assert(condition, msg) {
  if (condition) { console.log(`  ✅ ${msg}`); passed++; }
  else { console.log(`  ❌ FAIL: ${msg}`); failed++; }
}

console.log('\n🧪 Testing handleMove path computation logic\n');

// Test 1: Move root file into root folder
console.log('📝 Test 1: Root file → root folder');
let r = computeMoveParams('report.pdf', 'projects');
assert(r.from === 'report.pdf' && r.to === 'projects/report.pdf', 
  `report.pdf → projects: from=${r.from}, to=${r.to}`);

// Test 2: Move nested file to another root folder
console.log('\n📝 Test 2: Nested file → another root folder');
r = computeMoveParams('docs/notes.txt', 'archive');
assert(r.from === 'docs/notes.txt' && r.to === 'archive/notes.txt',
  `docs/notes.txt → archive: from=${r.from}, to=${r.to}`);

// Test 3: Move file out of folder to root (targetDirPath = '')
console.log('\n📝 Test 3: Nested file → root');
r = computeMoveParams('docs/notes.txt', '');
assert(r.from === 'docs/notes.txt' && r.to === 'notes.txt',
  `docs/notes.txt → root: from=${r.from}, to=${r.to}`);

// Test 4: Move folder into another folder
console.log('\n📝 Test 4: Folder → another folder');
r = computeMoveParams('folderA', 'folderB');
assert(r.from === 'folderA' && r.to === 'folderB/folderA',
  `folderA → folderB: from=${r.from}, to=${r.to}`);

// Test 5: Prevent moving folder into itself
console.log('\n📝 Test 5: Prevent folder into itself');
r = computeMoveParams('folderA', 'folderA');
assert(r.error !== undefined, `folderA → folderA: error=${r.error}`);

// Test 6: Prevent moving folder into its descendant
console.log('\n📝 Test 6: Prevent folder into descendant');
r = computeMoveParams('folderA', 'folderA/sub');
assert(r.error !== undefined, `folderA → folderA/sub: error=${r.error}`);

// Test 7: No-op when file already in target folder
console.log('\n📝 Test 7: No-op detection');
r = computeMoveParams('projects/report.pdf', 'projects');
assert(r.error === 'no-op', `projects/report.pdf → projects: error=${r.error}`);

// Test 8: Deep nested file move
console.log('\n📝 Test 8: Deep nested file move');
r = computeMoveParams('a/b/c/file.txt', 'x/y');
assert(r.from === 'a/b/c/file.txt' && r.to === 'x/y/file.txt',
  `a/b/c/file.txt → x/y: from=${r.from}, to=${r.to}`);

console.log(`\n${'='.repeat(50)}`);
console.log(`Results: ${passed} passed, ${failed} failed`);
console.log(`${'='.repeat(50)}`);

// Now test the actual API with the computed paths
console.log('\n🧪 Testing actual API calls with computed paths\n');

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const BASE = 'http://localhost:3000';
const WORKSPACE = path.join(process.env.HOME, '.jcowork/data/bea6ffbe-22dd-4114-baed-9c662b8f55d7/workspace');

const now = Math.floor(Date.now() / 1000);
const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
const payload = Buffer.from(JSON.stringify({
  sub: 'bea6ffbe-22dd-4114-baed-9c662b8f55d7', username: 'jhx', iat: now, exp: now + 86400
})).toString('base64url');
const sig = crypto.createHmac('sha256', 'change-me-in-production').update(header + '.' + payload).digest('base64url');
const TOKEN = header + '.' + payload + '.' + sig;
const auth = { 'Authorization': `Bearer ${TOKEN}`, 'Content-Type': 'application/json' };

async function api(method, urlPath, body) {
  const opts = { method, headers: auth };
  if (body) opts.body = JSON.stringify(body);
  const res = await fetch(`${BASE}${urlPath}`, opts);
  return { status: res.status, data: await res.json().catch(() => null) };
}

async function listFiles(p) {
  const res = await fetch(`${BASE}/api/workspace/files?path=${encodeURIComponent(p)}`, { headers: auth });
  return res.ok ? await res.json() : [];
}

async function testApiMoves() {
  // Cleanup
  await api('POST', '/api/workspace/delete', { path: 'drag_test' });
  
  // Setup: create folder structure
  await api('POST', '/api/workspace/mkdir', { path: 'drag_test/src' });
  await api('POST', '/api/workspace/mkdir', { path: 'drag_test/dest' });
  fs.writeFileSync(path.join(WORKSPACE, 'drag_test/src/file.txt'), 'test');
  
  // Verify setup
  let srcFiles = await listFiles('drag_test/src');
  assert(srcFiles.some(f => f.name === 'file.txt'), 'Setup: file.txt exists in drag_test/src');

  // Test: Move file from src to dest (simulating drag from file row, drop on dest folder)
  const moveResult = computeMoveParams('drag_test/src/file.txt', 'drag_test/dest');
  assert(!moveResult.error, `Move params computed correctly: ${JSON.stringify(moveResult)}`);
  
  if (!moveResult.error) {
    const res = await api('POST', '/api/workspace/move', moveResult);
    assert(res.status === 200, `API move success: status=${res.status}`);
    
    srcFiles = await listFiles('drag_test/src');
    assert(!srcFiles.some(f => f.name === 'file.txt'), 'File no longer in src');
    
    let destFiles = await listFiles('drag_test/dest');
    assert(destFiles.some(f => f.name === 'file.txt'), 'File now in dest');
    
    // Test: Move file back to src (simulating drag out)
    const moveBack = computeMoveParams('drag_test/dest/file.txt', 'drag_test/src');
    const res2 = await api('POST', '/api/workspace/move', moveBack);
    assert(res2.status === 200, `Move back success: status=${res2.status}`);
    
    srcFiles = await listFiles('drag_test/src');
    assert(srcFiles.some(f => f.name === 'file.txt'), 'File back in src');
  }

  // Cleanup
  await api('POST', '/api/workspace/delete', { path: 'drag_test' });
  
  console.log(`\n${'='.repeat(50)}`);
  console.log(`Total: ${passed} passed, ${failed} failed`);
  console.log(`${'='.repeat(50)}\n`);
  
  process.exit(failed > 0 ? 1 : 0);
}

testApiMoves().catch(e => { console.error(e); process.exit(1); });
