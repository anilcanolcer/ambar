#!/usr/bin/env node
// Tests scripts/release.mjs --collect-only without building Kova: it makes
// fake bundler output in a temp folder (a signed Universal .app, its tarball,
// a DMG and an .exe), signs them with a throwaway updater key and checks the
// latest.json, SHA256SUMS.txt and failure messages the script produces.
// The real signing key and the Keychain are never touched.
//
//   npm run release:test            (add -- --keep to keep the temp folder)

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const RELEASE = path.join(ROOT, "scripts", "release.mjs");
const TAURI = path.join(ROOT, "node_modules", ".bin", "tauri");
const CONF = JSON.parse(fs.readFileSync(path.join(ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
const VERSION = CONF.version;
const KEY_PASSWORD = "test";

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kova-release-test-"));
const target = path.join(tmp, "target");
const releaseDir = path.join(tmp, "release");
const macBundle = path.join(target, "universal-apple-darwin", "release", "bundle");
const nsis = path.join(target, "x86_64-pc-windows-msvc", "release", "bundle", "nsis");
const app = path.join(macBundle, "macos", "Kova.app");
const tarball = path.join(macBundle, "macos", "Kova.app.tar.gz");
const dmg = path.join(macBundle, "dmg", `Kova_${VERSION}_universal.dmg`);
const setup = path.join(nsis, `Kova_${VERSION}_x64-setup.exe`);
const key = path.join(tmp, "key", "test.key");

let failures = 0;

function check(ok, label) {
  console.log(`${ok ? "ok  " : "FAIL"} ${label}`);
  if (!ok) failures++;
}

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", ...opts });
  if (r.status !== 0) throw new Error(`${cmd} ${args.join(" ")}\n${r.stdout ?? ""}${r.stderr ?? ""}`);
  return r.stdout;
}

/** Signs like `tauri build` does; `version` null leaves the signature unbound. */
function sign(file, version = VERSION) {
  fs.rmSync(`${file}.sig`, { force: true });
  const bind = version === null ? [] : ["--app-version", version];
  sh(TAURI, ["signer", "sign", "-f", key, "-p", KEY_PASSWORD, ...bind, file], { stdio: ["ignore", "pipe", "pipe"] });
}

function plist(version) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>kova</string>
	<key>CFBundleIdentifier</key>
	<string>com.example.kova-release-test</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>${version}</string>
</dict>
</plist>
`;
}

function makeTarball(version) {
  fs.writeFileSync(path.join(app, "Contents", "Info.plist"), plist(version));
  sh("/usr/bin/codesign", ["--force", "--deep", "-s", "-", app]);
  sh("/usr/bin/tar", ["-czf", tarball, "-C", path.dirname(app), "Kova.app"]);
  sign(tarball);
}

function makeFakes() {
  sh(TAURI, ["signer", "generate", "--ci", "-p", KEY_PASSWORD, "-w", key]);

  // A Universal .app with a tiny real program, ad-hoc signed like Kova's.
  const macos = path.join(app, "Contents", "MacOS");
  fs.mkdirSync(macos, { recursive: true });
  const c = path.join(tmp, "main.c");
  fs.writeFileSync(c, "int main(void) { return 0; }\n");
  sh("/usr/bin/clang", ["-arch", "arm64", "-arch", "x86_64", "-o", path.join(macos, "kova"), c]);
  makeTarball(VERSION);

  fs.mkdirSync(path.dirname(dmg), { recursive: true });
  sh("/usr/bin/hdiutil", ["create", "-quiet", "-srcfolder", app, "-volname", "Kova", "-format", "UDZO", "-ov", dmg]);

  // Just enough of a PE file: "MZ", the PE header offset at 0x3c, "PE\0\0".
  fs.mkdirSync(nsis, { recursive: true });
  const exe = Buffer.alloc(1024);
  exe.write("MZ", 0, "latin1");
  exe.writeUInt32LE(0x80, 0x3c);
  exe.write("PE\0\0", 0x80, "latin1");
  fs.writeFileSync(setup, exe);
  sign(setup);
}

function release(extraEnv = {}) {
  return spawnSync(process.execPath, [RELEASE, "--collect-only"], {
    encoding: "utf8",
    env: {
      ...process.env,
      KOVA_TARGET_DIR: target,
      KOVA_RELEASE_DIR: releaseDir,
      KOVA_UPDATER_PUBKEY: fs.readFileSync(`${key}.pub`, "utf8").trim(),
      ...extraEnv,
    },
  });
}

function expectFailure(label, pattern, extraEnv) {
  const r = release(extraEnv);
  const error = r.stderr.split("\n").find((line) => line.startsWith("Hata:")) ?? "no error";
  check(r.status !== 0 && pattern.test(r.stderr), `${label} (${error})`);
}

try {
  makeFakes();

  const r = release();
  check(r.status === 0, "collect-only succeeds");
  if (r.status !== 0) console.log(r.stdout, r.stderr);
  const out = path.join(releaseDir, `v${VERSION}`);
  const names = [
    `Kova_${VERSION}_universal.dmg`,
    `Kova_${VERSION}_universal.app.tar.gz`,
    `Kova_${VERSION}_universal.app.tar.gz.sig`,
    `Kova_${VERSION}_x64-setup.exe`,
    `Kova_${VERSION}_x64-setup.exe.sig`,
    "latest.json",
    "SHA256SUMS.txt",
    "notes.md",
  ];
  check(names.every((n) => fs.existsSync(path.join(out, n))), "all release files are written");

  const latest = JSON.parse(fs.readFileSync(path.join(out, "latest.json"), "utf8"));
  const base = `https://github.com/alpunlu12-commits/kova/releases/download/v${VERSION}`;
  const macSig = fs.readFileSync(`${tarball}.sig`, "utf8").trim();
  const winSig = fs.readFileSync(`${setup}.sig`, "utf8").trim();
  check(latest.version === VERSION, "latest.json version");
  check(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(latest.pub_date), "pub_date is RFC 3339");
  check(typeof latest.notes === "string" && latest.notes.length > 0 && !/[#*`]/.test(latest.notes), "notes are plain text");
  for (const k of ["darwin-aarch64", "darwin-x86_64", "darwin-aarch64-app", "darwin-x86_64-app"]) {
    const p = latest.platforms[k];
    check(p?.url === `${base}/Kova_${VERSION}_universal.app.tar.gz` && p.signature === macSig, `platforms.${k}`);
  }
  for (const k of ["windows-x86_64", "windows-x86_64-nsis"]) {
    const p = latest.platforms[k];
    check(p?.url === `${base}/Kova_${VERSION}_x64-setup.exe` && p.signature === winSig, `platforms.${k}`);
  }
  const sums = fs.readFileSync(path.join(out, "SHA256SUMS.txt"), "utf8").trim().split("\n");
  check(
    sums.length === 3 &&
      sums.every((line) => {
        const [hash, name] = line.split("  ");
        return hash === createHash("sha256").update(fs.readFileSync(path.join(out, name))).digest("hex");
      }),
    "SHA256SUMS.txt matches the files",
  );
  check(spawnSync("/usr/bin/shasum", ["-a", "256", "-c", "SHA256SUMS.txt"], { cwd: out }).status === 0, "shasum -c accepts SHA256SUMS.txt");
  check(/gh release create v\S+ --repo alpunlu12-commits\/kova --title 'Kova [^']+' --notes-file \S+ --latest/.test(r.stdout), "prints the gh command");
  check(!r.stdout.includes("Yayında:"), "does not publish without --publish");

  console.log(`\nlatest.json:\n${fs.readFileSync(path.join(out, "latest.json"), "utf8")}`);

  // Failure cases, each restored afterwards.
  fs.appendFileSync(setup, "x");
  expectFailure("a changed setup is rejected", /imzası geçersiz/);
  fs.truncateSync(setup, 1024);

  sign(setup, "0.0.1");
  expectFailure("a signature for another version is rejected", /0\.0\.1 sürümü için imzalanmış/);
  sign(setup, null);
  if (CONF.plugins?.updater?.requireSignedVersion === true) {
    expectFailure("an unbound signature is rejected (requireSignedVersion)", /sürüme bağlı değil/);
  } else {
    check(/sürüme bağlı değil/.test(release().stderr), "an unbound signature is reported");
  }
  sign(setup);

  makeTarball("0.0.1");
  expectFailure("a stale app in the tarball is rejected", /0\.0\.1 sürümü/);
  makeTarball(VERSION);

  const other = path.join(tmp, "other");
  sh(TAURI, ["signer", "generate", "--ci", "-p", KEY_PASSWORD, "-w", path.join(other, "o.key")]);
  expectFailure("another key's pubkey is rejected", /başka bir anahtarla/, {
    KOVA_UPDATER_PUBKEY: fs.readFileSync(path.join(other, "o.key.pub"), "utf8").trim(),
  });

  fs.renameSync(`${setup}.sig`, `${setup}.sig.off`);
  expectFailure("a missing .sig is reported", /Şu dosyalar yok/);
  fs.renameSync(`${setup}.sig.off`, `${setup}.sig`);

  check(release().status === 0, "succeeds again once everything is restored");
} finally {
  if (process.argv.includes("--keep")) console.log(`\nKept: ${tmp}`);
  else fs.rmSync(tmp, { recursive: true, force: true });
}

console.log(failures === 0 ? "\nAll release checks passed." : `\n${failures} check(s) failed.`);
process.exit(failures === 0 ? 0 : 1);
