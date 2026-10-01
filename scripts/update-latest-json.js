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
const tag = args[4] || process.env.GITHUB_REF_NAME || 'v1.0.0';
const version = tag.replace(/^v/, '');

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

function fetchExistingLatest(url) {
  return new Promise((resolve) => {
    https.get(url, { headers: { 'User-Agent': 'Node.js' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return fetchExistingLatest(res.headers.location).then(resolve);
      }
      if (res.statusCode !== 200) {
        return resolve(null);
      }
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => {
        try {
          resolve(JSON.parse(data));
        } catch (e) {
          resolve(null);
        }
      });
    }).on('error', () => resolve(null));
  });
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
    } catch (e) {
      console.warn("Could not parse existing local latest.json, starting fresh.");
    }
  } else {
    // Try fetching from release tag
    const remoteUrl = `https://github.com/${repo}/releases/download/${tag}/latest.json`;
    console.log(`Checking existing remote latest.json at: ${remoteUrl}`);
    const existing = await fetchExistingLatest(remoteUrl);
    if (existing && existing.platforms) {
      console.log("Merged with existing release latest.json");
      manifest = existing;
    }
  }

  manifest.version = version;
  manifest.pub_date = new Date().toISOString();
  if (!manifest.platforms) {
    manifest.platforms = {};
  }

  manifest.platforms[platformKey] = {
    signature: signature,
    url: `https://github.com/${repo}/releases/download/${tag}/${artifactFilename}`
  };

  fs.writeFileSync(latestJsonPath, JSON.stringify(manifest, null, 2), 'utf8');
  console.log(`Successfully updated ${latestJsonPath}:`);
  console.log(JSON.stringify(manifest, null, 2));
}

run().catch((err) => {
  console.error("Failed to generate latest.json:", err);
  process.exit(1);
});
