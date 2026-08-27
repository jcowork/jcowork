#!/usr/bin/env node
/**
 * End-to-end test for /api/workspace/move API
 * Tests: create files/folders, move file into folder, move file out, move folder into folder
 */

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const BASE = 'http://localhost:3000';
const WORKSPACE = path.join(process.env.HOME, '.jcowork/data/bea6ffbe-22dd-4114-baed-9c662b8f55d7/workspace');

// Generate JWT token
const now = Math.floor(Date.now() / 1000);
const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
const payload = Buffer.from(JSON.stringify({
  sub: 'bea6ffbe-22dd-4114-baed-9c662b8f55d7',
  username: 'jhx',
  iat: now,
  exp: now + 86400
})).toString('base64url');
const sig = crypto.createHmac('sha256', 'change-me-in-production').update(header + '.' + payload).digest('base64url');
const TOKEN = header + '.' + payload + '.' + sig;

const auth = { 'Authorization': `Bearer ${TOKEN}`, 'Content-Type': 'application/json' };

async function api(method, path, body) {
  const opts = { method, headers: auth };
  if (body) opts.body = JSON.stringify(body);
  const res = await fetch(`${BASE}${path}`, opts);
  const data = await res.json().catch(() => null);
  return { status: res.status, data };
}

async function listFiles(path) {
  const res = await fetch(`${BASE}/api/workspace/files?path=${encodeURIComponent(path)}`, { headers: auth });
  return res.ok ? await res.json() : [];
}

let passed = 0, failed = 0;

function assert(condition, msg) {
  if (condition) {
    console.log(`  ✅ ${msg}`);
    passed++;
  } else {
    console.log(`  ❌ FAIL: ${msg}`);
    failed++;
  }
}

async function runTests() {
  console.log('\n🧪 Testing /api/workspace/move API\n');

  // Cleanup any previous test artifacts
  await api('POST', '/api/workspace/delete', { path: 'test_move_file.txt' });
  await api('POST', '/api/workspace/delete', { path: 'test_move_folder' });
  await api('POST', '/api/workspace/delete', { path: 'test_dest_folder' });

  // Step 1: Create test file directly on filesystem
  console.log('📝 Step 1: Create test file and folders');
  fs.writeFileSync(path.join(WORKSPACE, 'test_move_file.txt'), 'hello world');
  assert(fs.existsSync(path.join(WORKSPACE, 'test_move_file.txt')), 'Create test file');

  // Step 2: Create test folders
  const mkdirRes1 = await api('POST', '/api/workspace/mkdir', { path: 'test_move_folder' });
  assert(mkdirRes1.status === 200, `Create test_move_folder: status=${mkdirRes1.status}`);

  const mkdirRes2 = await api('POST', '/api/workspace/mkdir', { path: 'test_dest_folder' });
  assert(mkdirRes2.status === 200, `Create test_dest_folder: status=${mkdirRes2.status}`);

  // Create a file inside test_move_folder
  fs.writeFileSync(path.join(WORKSPACE, 'test_move_folder/inner_file.txt'), 'inner content');
  assert(fs.existsSync(path.join(WORKSPACE, 'test_move_folder/inner_file.txt')), 'Create inner file');

  // Verify files exist
  let rootFiles = await listFiles('.');
  assert(rootFiles.some(f => f.name === 'test_move_file.txt'), 'test_move_file.txt exists in root');
  assert(rootFiles.some(f => f.name === 'test_move_folder'), 'test_move_folder exists in root');
  assert(rootFiles.some(f => f.name === 'test_dest_folder'), 'test_dest_folder exists in root');

  // Step 3: Move file INTO folder
  console.log('\n📝 Step 3: Move file into folder');
  const move1 = await api('POST', '/api/workspace/move', {
    from: 'test_move_file.txt',
    to: 'test_move_folder/test_move_file.txt'
  });
  assert(move1.status === 200, `Move file into folder: status=${move1.status}, response=${JSON.stringify(move1.data)}`);

  // Verify file moved
  rootFiles = await listFiles('.');
  assert(!rootFiles.some(f => f.name === 'test_move_file.txt'), 'File no longer in root');

  let folderFiles = await listFiles('test_move_folder');
  assert(folderFiles.some(f => f.name === 'test_move_file.txt'), 'File now in test_move_folder');

  // Step 4: Move file OUT of folder back to root
  console.log('\n📝 Step 4: Move file out of folder to root');
  const move2 = await api('POST', '/api/workspace/move', {
    from: 'test_move_folder/test_move_file.txt',
    to: 'test_move_file.txt'
  });
  assert(move2.status === 200, `Move file to root: status=${move2.status}, response=${JSON.stringify(move2.data)}`);

  rootFiles = await listFiles('.');
  assert(rootFiles.some(f => f.name === 'test_move_file.txt'), 'File back in root');

  folderFiles = await listFiles('test_move_folder');
  assert(!folderFiles.some(f => f.name === 'test_move_file.txt'), 'File no longer in folder');

  // Step 5: Move file into another folder
  console.log('\n📝 Step 5: Move file into test_dest_folder');
  const move3 = await api('POST', '/api/workspace/move', {
    from: 'test_move_file.txt',
    to: 'test_dest_folder/test_move_file.txt'
  });
  assert(move3.status === 200, `Move to dest folder: status=${move3.status}`);

  let destFiles = await listFiles('test_dest_folder');
  assert(destFiles.some(f => f.name === 'test_move_file.txt'), 'File in test_dest_folder');

  // Step 6: Move folder into another folder (move test_move_folder into test_dest_folder)
  console.log('\n📝 Step 6: Move folder into another folder');
  const move4 = await api('POST', '/api/workspace/move', {
    from: 'test_move_folder',
    to: 'test_dest_folder/test_move_folder'
  });
  assert(move4.status === 200, `Move folder: status=${move4.status}, response=${JSON.stringify(move4.data)}`);

  destFiles = await listFiles('test_dest_folder');
  assert(destFiles.some(f => f.name === 'test_move_folder' && f.type === 'dir'), 'test_move_folder inside test_dest_folder');

  // Verify inner file is accessible
  let nestedFiles = await listFiles('test_dest_folder/test_move_folder');
  assert(nestedFiles.some(f => f.name === 'inner_file.txt'), 'Inner file still accessible after folder move');

  // Step 7: Test invalid moves
  console.log('\n📝 Step 7: Test invalid moves (should fail)');

  // Move folder into itself
  const badMove1 = await api('POST', '/api/workspace/move', {
    from: 'test_dest_folder',
    to: 'test_dest_folder/test_dest_folder'
  });
  assert(badMove1.status !== 200, `Move folder into itself rejected: status=${badMove1.status}`);

  // Move to path with ..
  const badMove2 = await api('POST', '/api/workspace/move', {
    from: 'test_dest_folder/test_move_file.txt',
    to: '../escape.txt'
  });
  assert(badMove2.status !== 200, `Path traversal rejected: status=${badMove2.status}`);

  // Cleanup
  console.log('\n📝 Cleanup');
  await api('POST', '/api/workspace/delete', { path: 'test_dest_folder' });
  rootFiles = await listFiles('.');
  assert(!rootFiles.some(f => f.name === 'test_dest_folder'), 'Cleanup: test_dest_folder removed');
  assert(!rootFiles.some(f => f.name === 'test_move_folder'), 'Cleanup: test_move_folder removed');
  assert(!rootFiles.some(f => f.name === 'test_move_file.txt'), 'Cleanup: test_move_file.txt removed');

  console.log(`\n${'='.repeat(50)}`);
  console.log(`Results: ${passed} passed, ${failed} failed`);
  console.log(`${'='.repeat(50)}\n`);

  process.exit(failed > 0 ? 1 : 0);
}

runTests().catch(e => {
  console.error('Test error:', e);
  process.exit(1);
});
