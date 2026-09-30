const fs = require('fs');
const path = require('path');

const base = 'D:\\Open app remote software';
const main = path.join(base, 'app-ui', 'src-tauri', 'gen', 'android', 'app', 'src', 'main');

// Create jniLibs directories
const targets = [
  { src: 'target/aarch64-linux-android/release/libopen_remote_gui_lib.so', dst: 'jniLibs/arm64-v8a/libopen_remote_gui_lib.so' },
  { src: 'target/armv7-linux-androideabi/release/libopen_remote_gui_lib.so', dst: 'jniLibs/armeabi-v7a/libopen_remote_gui_lib.so' },
  { src: 'target/x86_64-linux-android/release/libopen_remote_gui_lib.so', dst: 'jniLibs/x86_64/libopen_remote_gui_lib.so' }
];

for (const t of targets) {
  const srcPath = path.join(base, t.src);
  const dstPath = path.join(main, t.dst);
  fs.mkdirSync(path.dirname(dstPath), { recursive: true });
  fs.copyFileSync(srcPath, dstPath);
  console.log(`Copied ${t.src} -> ${t.dst} (${fs.statSync(dstPath).size} bytes)`);
}

// Copy build frontend assets
const buildDir = path.join(base, 'app-ui', 'build');
const assetsDir = path.join(main, 'assets');
fs.mkdirSync(assetsDir, { recursive: true });

function copyDir(src, dst) {
  for (const item of fs.readdirSync(src)) {
    const s = path.join(src, item);
    const d = path.join(dst, item);
    if (fs.statSync(s).isDirectory()) {
      fs.mkdirSync(d, { recursive: true });
      copyDir(s, d);
    } else {
      fs.copyFileSync(s, d);
    }
  }
}

copyDir(buildDir, assetsDir);
console.log('Copied all frontend assets to android assets directory.');
