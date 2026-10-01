#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const https = require('https');

// Usage: node scripts/update-latest-json.js <platformKey> <sigFilePath> <artifactFilename> [repo] [tag]
// Example: node scripts/update-latest-json.js darwin-x86_64 artifacts/OpenRemote-Intel-x64.app.tar.gz.sig OpenRemote-Intel-x64.app.tar.gz

const args = process.argv.slice(2);
if (args.length < 3) {
  console.error("Usage: node scripts/update-latest-json.js <platformKey> <sigFilePath> <artifactFilename> [repo] [tag]");
  process.exit(1);
}

const platformKey = args[0];
const sigFilePath = args[1];
const artifactFilename = args[2];
const repo = args[3] || process.env.GITHUB_REPOSITORY || 'storyboxvasu-sudo/Open-Remote';

// Read default version from tauri.conf.json if available
let defaultVersion = '1.0.0';
try {
  const confPath = path.resolve(__dirname, '../app-ui/src-tauri/tauri.conf.json');
  if (fs.existsSync(confPath)) {
    const conf = JSON.parse(fs.readFileSync(confPath, 'utf8'));
    if (conf.version) defaultVersion = conf.version;
  }
} catch (e) {}

const rawTag = args[4] || process.env.GITHUB_REF_NAME || `v${defaultVersion}`;
const tag = rawTag.startsWith('v') ? rawTag : `v${rawTag}`;
const version = rawTag.startsWith('v') ? rawTag.replace(/^v/, '') : (rawTag === 'main' || rawTag === 'master' ? defaultVersion : rawTag);

if (!fs.existsSync(sigFilePath)) {
  console.error(`Signature file not found: ${sigFilePath}`);
  process.exit(1);
}

const signature = fs.readFileSync(sigFilePath, 'utf8').trim();
const artifactsDir = path.resolve('artifacts');
if (!fs.existsSync(artifactsDir)) {
  fs.mkdirSync(artifactsDir, { recursive: true });
}

const latestJsonPath = path.join(artifactsDir, 'latest.json');

function httpsGet(url, customHeaders = {}) {
  return new Promise((resolve) => {
    const headers = {
      'User-Agent': 'OpenRemote-Updater-Script',
      ...customHeaders
    };
    https.get(url, { headers }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return httpsGet(res.headers.location, customHeaders).then(resolve);
      }
      if (res.statusCode !== 200) {
        return resolve({ status: res.statusCode, data: null });
      }
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => resolve({ status: 200, data }));
    }).on('error', () => resolve({ status: 500, data: null }));
  });
}

async function fetchExistingLatest() {
  const authHeader = process.env.GITHUB_TOKEN ? { 'Authorization': `Bearer ${process.env.GITHUB_TOKEN}` } : {};

  // 1. Direct release download URL for this tag
  const directUrl = `https://github.com/${repo}/releases/download/${tag}/latest.json`;
  console.log(`Checking direct release URL: ${directUrl}`);
  const directRes = await httpsGet(directUrl, authHeader);
  if (directRes.status === 200 && directRes.data) {
    try {
      return JSON.parse(directRes.data);
    } catch (e) {}
  }

  // 2. Query GitHub Releases API for the tag assets
  const apiUrl = `https://api.github.com/repos/${repo}/releases/tags/${tag}`;
  console.log(`Checking GitHub Releases API: ${apiUrl}`);
  const apiRes = await httpsGet(apiUrl, authHeader);
  if (apiRes.status === 200 && apiRes.data) {
    try {
      const releaseInfo = JSON.parse(apiRes.data);
      const latestAsset = (releaseInfo.assets || []).find(a => a.name === 'latest.json');
      if (latestAsset && latestAsset.browser_download_url) {
        console.log(`Found latest.json asset at: ${latestAsset.browser_download_url}`);
        const assetRes = await httpsGet(latestAsset.browser_download_url, authHeader);
        if (assetRes.status === 200 && assetRes.data) {
          return JSON.parse(assetRes.data);
        }
      }
    } catch (e) {}
  }

  // 3. Fallback to releases/latest download
  const latestUrl = `https://github.com/${repo}/releases/latest/download/latest.json`;
  console.log(`Checking latest download URL: ${latestUrl}`);
  const latestRes = await httpsGet(latestUrl, authHeader);
  if (latestRes.status === 200 && latestRes.data) {
    try {
      return JSON.parse(latestRes.data);
    } catch (e) {}
  }

  return null;
}

async function run() {
  let manifest = {
    version: version,
    notes: `OpenRemote Release ${tag}`,
    pub_date: new Date().toISOString(),
    platforms: {}
  };

  if (fs.existsSync(latestJsonPath)) {
    try {
      manifest = JSON.parse(fs.readFileSync(latestJsonPath, 'utf8'));
      console.log(`Loaded existing local ${latestJsonPath}`);
    } catch (e) {
      console.warn("Could not parse existing local latest.json, will check remote.");
    }
  }

  // If local manifest has no platforms yet, try fetching existing remote manifest
  if (!manifest.platforms || Object.keys(manifest.platforms).length === 0) {
    const existing = await fetchExistingLatest();
    if (existing && existing.platforms) {
      console.log("Merged with existing remote latest.json platforms:", Object.keys(existing.platforms));
      manifest = existing;
    }
  }

  manifest.version = version;
  manifest.pub_date = new Date().toISOString();
  if (!manifest.platforms) {
    manifest.platforms = {};
  }

  const platformEntry = {
    signature: signature,
    url: `https://github.com/${repo}/releases/download/${tag}/${artifactFilename}`
  };

  // Populate primary platform key and compatibility aliases
  manifest.platforms[platformKey] = platformEntry;

  if (platformKey === 'windows-x86_64') {
    manifest.platforms['windows-x64'] = platformEntry;
    manifest.platforms['x86_64-pc-windows-msvc'] = platformEntry;
  } else if (platformKey === 'darwin-aarch64') {
    manifest.platforms['darwin-arm64'] = platformEntry;
    manifest.platforms['aarch64-apple-darwin'] = platformEntry;
  } else if (platformKey === 'darwin-x86_64') {
    manifest.platforms['x86_64-apple-darwin'] = platformEntry;
  }

  fs.writeFileSync(latestJsonPath, JSON.stringify(manifest, null, 2), 'utf8');
  console.log(`Successfully generated and saved ${latestJsonPath}:`);
  console.log(JSON.stringify(manifest, null, 2));
}

run().catch((err) => {
  console.error("Failed to generate latest.json:", err);
  process.exit(1);
});
