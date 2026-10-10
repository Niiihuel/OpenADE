#!/usr/bin/env node
// Always scope EAS to this native app, never to the enclosing Git repository.
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const [action = 'inspect', profile = 'simulator', ...extra] = process.argv.slice(2);
if (!['inspect', 'build', 'init', 'credentials', 'submit'].includes(action) || !['simulator', 'device', 'testflight'].includes(profile)) {
  throw new Error('Usage: node scripts/eas.mjs [inspect|build|init|credentials|submit] [simulator|device|testflight] [EAS flags]');
}
if (extra.some(flag => flag === '-s' || flag.startsWith('--auto-submit'))) {
  throw new Error('Automatic submission is disabled. Submit an explicitly selected store build using the submit command.');
}
function run(command, args, projectRoot = root) {
  // EAS otherwise creates a group and invites every App Store Connect admin.
  // Keep access assignment explicit and scoped to the account owner.
  const env = {...process.env, EAS_NO_VCS: '1', EAS_PROJECT_ROOT: projectRoot, EXPO_NO_DOTENV: '1', EAS_NO_AUTO_TESTFLIGHT_SETUP: '1'};
  const result = spawnSync(command, args, {cwd: projectRoot, env, stdio: 'inherit'});
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
function allowed(relative) {
  return /^(package\.json|package-lock\.json|app\.json|eas\.json|\.easignore)$/.test(relative)
    || /^\.eas\/build\/[^/]+\.yml$/.test(relative)
    || /^scripts\/[^/]+\.(py|sh|mjs)$/.test(relative)
    || /^ios\/OpenADERemote\.xcodeproj\/(project\.pbxproj|xcshareddata\/xcschemes\/[^/]+\.xcscheme)$/.test(relative)
    || /^OpenADERemote\/(?:[^/]+\/)*[^/]+\.swift$/.test(relative)
    || ['OpenADERemote/Info.plist', 'OpenADERemote/Simulator.entitlements', 'OpenADERemote/OpenADERemote.entitlements', 'OpenADERemote/PrivacyInfo.xcprivacy'].includes(relative)
    || /^OpenADERemote\/Assets\.xcassets\/(?:[^/]+\/)*[^/]+\.(json|png)$/.test(relative)
    || /^OpenADERemoteTests\/(?:[^/]+\/)*[^/]+\.swift$/.test(relative);
}
function inspect() {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'openade-ios-archive-'));
  const output = path.join(parent, 'archive');
  run('eas', ['build:inspect', '--platform', 'ios', '--profile', profile, '--stage', 'archive', '--output', output]);
  const files = [];
  function walk(dir) {
    for (const entry of fs.readdirSync(dir, {withFileTypes: true})) {
      const full = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`Archive contains symlink: ${full}`);
      if (entry.isDirectory()) walk(full);
      else files.push(path.relative(output, full).split(path.sep).join('/'));
    }
  }
  walk(output);
  const rejected = files.filter(file => !allowed(file));
  if (rejected.length) throw new Error(`Unexpected archive files: ${rejected.join(', ')}`);
  if (!files.some(file => file.endsWith('/OpenADERemoteApp.swift'))) throw new Error('App entry point missing from archive');
  if (profile === 'testflight') {
    for (const required of ['OpenADERemote/PrivacyInfo.xcprivacy', 'OpenADERemote/Assets.xcassets/AppIcon.appiconset/Contents.json']) {
      if (!files.includes(required)) throw new Error(`Store build resource missing: ${required}`);
    }
    if (!files.some(file => /^OpenADERemote\/Assets\.xcassets\/AppIcon\.appiconset\/[^/]+\.png$/.test(file))) {
      throw new Error('Store build app icon PNG missing');
    }
  }
  for (const file of files.filter(file => !file.endsWith('.png'))) {
    const contents = fs.readFileSync(path.join(output, file), 'utf8');
    // Fail without printing matched contents. This supplements the path allowlist.
    if (/-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/.test(contents)
      || /(?:sk-proj-|sk-ant-api)[A-Za-z0-9_-]{24,}/.test(contents)) {
      throw new Error(`Possible private credential in archive file: ${file}`);
    }
  }
  const size = files.reduce((total, file) => total + fs.statSync(path.join(output, file)).size, 0);
  console.log(`Verified native-only archive: ${files.length} files, ${size} bytes. ${output}`);
  console.log(files.sort().join('\n'));
  return output;
}
if (action === 'submit') {
  if (profile !== 'testflight') throw new Error('Only the explicit testflight submission profile is supported');
  const idIndex = extra.indexOf('--id');
  const buildId = idIndex >= 0 ? extra[idIndex + 1] : undefined;
  if (!buildId || !/^[a-f0-9-]{36}$/.test(buildId)) throw new Error('Submission requires --id with a completed store build UUID');
  if (extra.some(flag => /^(?:--latest|--path|--url|--platform|--profile)(?:=|$)/.test(flag) || ['-p', '-e'].includes(flag))) {
    throw new Error('Submission target overrides are disabled; use the explicit build ID');
  }
  const details = spawnSync('eas', ['build:view', buildId, '--json'], {cwd: root, env: {...process.env, EAS_NO_VCS: '1', EAS_PROJECT_ROOT: root, EXPO_NO_DOTENV: '1'}, encoding: 'utf8'});
  if (details.status !== 0) throw new Error('Could not verify the selected EAS build');
  const build = JSON.parse(details.stdout);
  const projectId = JSON.parse(fs.readFileSync(path.join(root, 'app.json'), 'utf8')).expo.extra.eas.projectId;
  if (build.status !== 'FINISHED' || build.distribution !== 'STORE' || build.project?.id !== projectId || build.buildProfile !== 'testflight') {
    throw new Error('Selected build must be a finished store build from this project and testflight profile');
  }
  run('eas', ['submit', '--platform', 'ios', '--profile', 'testflight', ...extra]);
} else if (action === 'init') {
  run('eas', ['init', ...extra]);
} else if (action === 'credentials') {
  run('python3', ['scripts/generate_project.py']);
  run('eas', ['credentials', '--platform', 'ios', ...extra]);
} else {
  run('python3', ['scripts/generate_project.py']);
  const inspectedSnapshot = inspect();
  // Upload the inspected snapshot, so concurrent source edits cannot change it.
  if (action === 'build') run('eas', ['build', '--platform', 'ios', '--profile', profile, ...extra], inspectedSnapshot);
}
