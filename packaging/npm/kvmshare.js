#!/usr/bin/env node
// kvmshare npm bootstrap: `npx github:YELrhilassi/kvmshare` (or `npm exec
// kvmshare` once published) runs the same verified bootstrap as the
// shell/PowerShell one-liners - resolve the release, verify a download
// against the release's digests, hand off to the Go installer.
//
// This file deliberately does NOT install anything itself: installation
// lives in kvmshare-install (download + verify + atomic apply + desktop
// integration). Node's https is used directly so Windows needs nothing
// extra (no curl). Works on every release: standalone-installer asset
// when present (v0.8.8+), otherwise the platform archive with --local.
//
// Usage:
//   npx github:YELrhilassi/kvmshare              install the latest release
//   npx github:YELrhilassi/kvmshare v0.8.7       pin a version
//   npx github:YELrhilassi/kvmshare -- --uninstall
//   KVMSHARE_UPSTREAM=owner/repo npx github:...

'use strict';

const https = require('https');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');

const REPO = process.env.KVMSHARE_UPSTREAM || 'YELrhilassi/kvmshare';
const API = `https://api.github.com/repos/${REPO}/releases`;
const HEADERS = { 'User-Agent': 'kvmshare-bootstrap', Accept: 'application/vnd.github+json' };

const say = (msg) => console.log(`kvmshare-install: ${msg}`);
const die = (msg) => { console.error(`kvmshare-install: ${msg}`); process.exit(1); };

// Minimal https GET (no dependencies): follows redirects, buffers fully
// (assets are tens of MB at most).
function get(url, redirects = 5) {
  return new Promise((resolve, reject) => {
    if (redirects <= 0) return reject(new Error('too many redirects'));
    https.get(url, { headers: HEADERS }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        return resolve(get(new URL(res.headers.location, url).toString(), redirects - 1));
      }
      if (res.statusCode !== 200) {
        res.resume();
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve(Buffer.concat(chunks)));
      res.on('error', reject);
    }).on('error', reject);
  });
}

function platformPlat() {
  const { platform, arch } = process;
  if (platform === 'linux' && arch === 'x64') return 'linux_amd64';
  if (platform === 'linux' && arch === 'arm64') return 'linux_arm64';
  if (platform === 'darwin' && arch === 'arm64') return 'darwin_arm64';
  if (platform === 'darwin' && arch === 'x64') return 'darwin_amd64';
  if (platform === 'win32' && arch === 'x64') return 'windows_amd64';
  die(`unsupported platform ${platform}-${arch}`);
}

function sha256(buf) {
  return crypto.createHash('sha256').update(buf).digest('hex');
}

// Preferred: the release's SHA256SUMS; fallback: the API digest field.
function expectedHash(assetName, digests, tag) {
  return get(`https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS`)
    .then((buf) => {
      for (const line of buf.toString('utf8').split('\n')) {
        const m = line.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
        if (m && m[2].trim() === assetName) return m[1];
      }
      return digests[assetName] || null;
    })
    .catch(() => digests[assetName] || null);
}

async function main() {
  const argv = process.argv.slice(2);
  const tagArg = argv.find((a) => /^v?\d/.test(a)) || '';
  const passThrough = argv.filter((a) => a !== tagArg);
  const plat = platformPlat();
  const exeSuffix = process.platform === 'win32' ? '.exe' : '';

  let tag = tagArg;
  if (!tag) {
    say('resolving the latest release...');
    const rel = JSON.parse((await get(`${API}/latest`)).toString('utf8'));
    tag = rel.tag_name;
    if (!tag) die('could not resolve the latest release (GitHub unreachable or rate-limited?)');
  }
  say(`installing ${tag}`);

  const relUrl = tagArg
    ? `${API}/tags/${tag}`
    : `${API}/latest`;
  const rel = JSON.parse((await get(relUrl)).toString('utf8'));
  const digests = {};
  for (const a of rel.assets || []) {
    const m = (a.digest || '').match(/^sha256:([0-9a-f]{64})$/);
    if (m) digests[a.name] = m[1];
  }

  const dl = `https://github.com/${REPO}/releases/download/${tag}`;
  const installerAsset = `kvmshare-install_${tag}_${plat}${exeSuffix}`;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmshare-install-'));
  const installerPath = path.join(tmp, 'kvmshare-install' + exeSuffix);
  // The asset the installer bytes came from: the standalone asset
  // (verified below) or null when they came from the already-verified
  // archive (the archive hash covered every file inside it).
  let assetName = null;

  const standalone = (rel.assets || []).find((a) => a.name === installerAsset);
  if (standalone) {
    say(`downloading ${installerAsset}...`);
    fs.writeFileSync(installerPath, await get(`${dl}/${installerAsset}`), { mode: 0o755 });
    assetName = installerAsset;
  } else {
    say('this release has no standalone installer asset - using the full archive');
    const archiveAsset = `kvmshare_${tag}_${plat}.tar.gz`;
    const isWin = process.platform === 'win32';
    if (isWin) die('Windows releases without a standalone installer predate this script; use install.ps1 (docs 9.7)');
    const archiveAsset2 = archiveAsset;
    say(`downloading ${archiveAsset2}...`);
    const buf = await get(`${dl}/${archiveAsset2}`);
    const want = await expectedHash(archiveAsset2, digests, tag);
    if (!want) die(`no trusted digest for ${archiveAsset2}`);
    if (sha256(buf) !== want) {
      die(`checksum mismatch for ${archiveAsset2}\n  expected: ${want}\n  got:      ${sha256(buf)}\nThe release may be corrupted, or this download was tampered with.`);
    }
    say(`checksum ok (${archiveAsset2})`);
    // Extract just the installer; --local points it at the extracted dir.
    const { execFileSync } = require('child_process');
    execFileSync('tar', ['-xzf', '-'], { cwd: tmp, input: buf, stdio: ['pipe', 'ignore', 'inherit'] });
    const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
      const p = path.join(dir, e.name);
      return e.isDirectory() ? walk(p) : [p];
    });
    const found = walk(tmp).find((p) => path.basename(p) === 'kvmshare-install');
    if (!found) die('archive has no kvmshare-install inside - unexpected layout');
    fs.chmodSync(found, 0o755);
    fs.copyFileSync(found, installerPath);
    // Install from the extracted archive (offline): the bytes are the
    // release's own, already hash-verified above.
    passThrough.push('--local', path.dirname(found));
  }

  if (assetName) {
    const want = await expectedHash(assetName, digests, tag);
    if (!want) die(`no trusted digest for ${assetName}`);
    const got = sha256(fs.readFileSync(installerPath));
    if (got !== want) {
      die(`checksum mismatch for ${assetName}\n  expected: ${want}\n  got:      ${got}\nThe release may be corrupted, or this download was tampered with.`);
    }
    say(`checksum ok (${assetName})`);
  }
  say('handing off to the installer...');

  const child = spawn(installerPath, passThrough, { stdio: 'inherit' });
  child.on('exit', (code) => {
    fs.rmSync(tmp, { recursive: true, force: true });
    process.exit(code || 0);
  });
  child.on('error', (err) => die(`running the installer failed: ${err.message}`));
}

main().catch((err) => die(err.message));
