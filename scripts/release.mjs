throw new Error("Ambar için yayınlama yapılandırılmadı. Önce kendi deponuzu ve imza anahtarınızı ayarlayın.");
#!/usr/bin/env node
// Builds and collects a Kova release on this Mac: the Universal macOS DMG and
// the Windows x64 setup, their updater signatures, latest.json (what the
// in-app "Güncelle" reads) and SHA256SUMS.txt, all in release/v<version>/.
//
// It never pushes anything. It prints the `gh release create` command, and
// runs it only with --publish, after the owner types a confirmation.
//
//   npm run release                     build both, then collect
//   npm run release -- --collect-only   collect what is already built
//   npm run release -- --publish        also create the GitHub release
//
// Needs Node 20+ and no npm packages. The updater key stays where it is:
// the CLI gets its path and the Keychain password through the child's
// environment only, and neither is ever printed.
//
// For testing (scripts/release-test.mjs): KOVA_TARGET_DIR replaces
// src-tauri/target, KOVA_RELEASE_DIR replaces release/, and
// KOVA_UPDATER_PUBKEY replaces the pubkey in tauri.conf.json.

import { spawnSync } from "node:child_process";
import { createHash, createPublicKey, verify } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import readline from "node:readline/promises";
import { fileURLToPath } from "node:url";

const REPO = "alpunlu12-commits/kova";
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const TARGET_DIR = path.resolve(process.env.KOVA_TARGET_DIR || path.join(ROOT, "src-tauri", "target"));
const RELEASE_DIR = path.resolve(process.env.KOVA_RELEASE_DIR || path.join(ROOT, "release"));
const KEY_PATH = path.join(os.homedir(), ".tauri", "kova.key");
const KEYCHAIN_SERVICE = "kova-updater-key";
const MAC_TARGET = "universal-apple-darwin";
const WIN_TARGET = "x86_64-pc-windows-msvc";
const RUST_TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin", WIN_TARGET];
/** The same PATH additions the "tauri" and "tauri:win" npm scripts make. */
const TOOL_PATH = [
  "/opt/homebrew/opt/llvm/bin",
  "/opt/homebrew/opt/lld/bin",
  "/opt/homebrew/opt/rustup/bin",
  path.join(os.homedir(), ".cargo", "bin"),
].join(":");

class Failure extends Error {}

function fail(message) {
  throw new Failure(message);
}

function warn(message) {
  console.warn(`! ${message}`);
}

function step(message) {
  console.log(`\n== ${message}`);
}

const USAGE = `Kova sürüm betiği

  npm run release                     iki sürümü derler, release/v<sürüm>/ içine toplar
  npm run release -- --collect-only   derlemeden, hazır dosyaları toplar
  npm run release -- --publish        sonunda GitHub sürümünü de oluşturur (önce sorar)`;

// ------------------------------------------------------------------ inputs

function readConfig() {
  const conf = JSON.parse(fs.readFileSync(path.join(ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
  const version = conf.version;
  if (typeof version !== "string" || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
    fail(`tauri.conf.json içindeki sürüm geçersiz: ${version}`);
  }
  // "v1Compatible" makes .zip updater bundles instead of signing the
  // installer itself; latest.json below assumes the v2 layout.
  if (conf.bundle?.createUpdaterArtifacts !== true) {
    fail('tauri.conf.json: bundle.createUpdaterArtifacts true olmalı, yoksa güncelleme imzaları üretilmez.');
  }
  const pubkey = process.env.KOVA_UPDATER_PUBKEY || conf.plugins?.updater?.pubkey;
  if (!pubkey) fail("tauri.conf.json: plugins.updater.pubkey yok.");
  if (process.env.KOVA_UPDATER_PUBKEY) warn("KOVA_UPDATER_PUBKEY kullanılıyor (yalnızca deneme için).");
  const endpoint = `https://github.com/${REPO}/releases/latest/download/latest.json`;
  if (!conf.plugins?.updater?.endpoints?.includes(endpoint)) {
    warn(`plugins.updater.endpoints içinde ${endpoint} yok, uygulama bu sürümü bulamayabilir.`);
  }
  // The app then refuses signatures that are not bound to the version.
  const signedVersionRequired = conf.plugins?.updater?.requireSignedVersion === true;
  // Kova refuses an update that needs a newer macOS than the Mac it runs on
  // (update.rs, runs_on_this_mac), instead of installing one that won't open.
  const macosMinimum = conf.bundle?.macOS?.minimumSystemVersion;
  if (!macosMinimum) fail("tauri.conf.json: bundle.macOS.minimumSystemVersion yok.");
  return { version, pubkey, signedVersionRequired, macosMinimum };
}

/** package.json and Cargo.toml must carry the same version as tauri.conf.json. */
function checkVersions(version) {
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
  const cargo = fs.readFileSync(path.join(ROOT, "src-tauri", "Cargo.toml"), "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const wrong = [
    pkg !== version && `package.json (${pkg})`,
    cargo !== version && `src-tauri/Cargo.toml (${cargo})`,
  ].filter(Boolean);
  if (wrong.length > 0) fail(`Sürüm ${version} olmalı: ${wrong.join(", ")}`);
}

function readNotes(version) {
  const file = path.join(ROOT, "release-notes", `${version}.md`);
  if (!fs.existsSync(file)) {
    fail(`Sürüm notu yok: release-notes/${version}.md\nBu dosyayı yaz (kısa, Türkçe), sonra tekrar çalıştır.`);
  }
  const text = fs.readFileSync(file, "utf8").trim();
  if (!text) fail(`release-notes/${version}.md boş.`);
  return text;
}

/** The in-app update notice shows plain text, so Markdown marks go. */
function plainText(markdown) {
  return markdown
    .replace(/<!--[\s\S]*?-->/g, "")
    .split(/\r?\n/)
    .map((line) =>
      line
        .replace(/^#{1,6}\s+/, "")
        .replace(/^(\s*)[-*+]\s+/, "$1• ")
        .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
        .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
        .replace(/(\*\*|__)(.+?)\1/g, "$2")
        .replace(/`([^`]+)`/g, "$1"),
    )
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

// ------------------------------------------------------------------- build

function toolEnv(extra = {}) {
  return { ...process.env, PATH: `${TOOL_PATH}:${process.env.PATH ?? ""}`, ...extra };
}

/** Runs a command quietly; returns its output or null when it cannot run. */
function capture(cmd, args, env = toolEnv()) {
  const r = spawnSync(cmd, args, { encoding: "utf8", env, stdio: ["ignore", "pipe", "pipe"] });
  return r.status === 0 ? r.stdout : null;
}

function run(label, cmd, args, env) {
  console.log(`$ ${[cmd, ...args].map(shellQuote).join(" ")}`);
  const r = spawnSync(cmd, args, { cwd: ROOT, env, stdio: "inherit" });
  if (r.error) fail(`${label} başlatılamadı: ${r.error.message}`);
  if (r.status !== 0) fail(`${label} başarısız oldu (çıkış kodu ${r.status}).`);
}

/** Catches missing tools before a long build instead of after it. */
function preflight() {
  const installed = capture("rustup", ["target", "list", "--installed"]);
  if (installed === null) fail("rustup bulunamadı.");
  const missing = RUST_TARGETS.filter((t) => !installed.split(/\s+/).includes(t));
  if (missing.length > 0) fail(`Eksik Rust hedefleri: ${missing.join(", ")}\nKur: rustup target add ${missing.join(" ")}`);
  if (capture("cargo", ["xwin", "--version"]) === null) fail("cargo-xwin yok. Kur: cargo install cargo-xwin");
  if (capture("makensis", ["-VERSION"]) === null) fail("makensis yok. Kur: brew install nsis");
  if (capture("lld-link", ["--version"]) === null) fail("lld-link yok. Kur: brew install llvm lld");
  if (!fs.existsSync(KEY_PATH)) fail(`Güncelleme imza anahtarı yok: ${KEY_PATH}`);
  try {
    const s = fs.statfsSync(ROOT);
    const freeGb = (s.bavail * s.bsize) / 1e9;
    if (freeGb < 10) warn(`Diskte ${freeGb.toFixed(1)} GB boş yer var; iki derleme için az olabilir.`);
  } catch {
    // Only a hint; the build reports a full disk itself.
  }
}

/** The key's password, from the Keychain. Never printed or logged. */
function signingPassword() {
  const r = spawnSync("/usr/bin/security", ["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  });
  if (r.status !== 0 || !r.stdout) {
    fail(`İmza anahtarının şifresi Anahtar Zinciri'nde bulunamadı (servis adı: ${KEYCHAIN_SERVICE}).`);
  }
  return r.stdout.replace(/\r?\n$/, "");
}

function build() {
  step("Ön kontrol");
  preflight();
  // `tauri build` and `bundle` accept a key file path here (the CLI's own
  // help says so), so the key text never passes through this process.
  // `tauri build` also binds each signature to the app version by itself.
  // PATH stays as it is: the npm scripts add what each build needs, and
  // Homebrew's clang must not shadow Apple's in the macOS build.
  const env = {
    ...process.env,
    TAURI_SIGNING_PRIVATE_KEY: KEY_PATH,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: signingPassword(),
  };
  // Anything older than this is left over from an earlier build.
  const started = Date.now() - 2000;

  step("macOS Universal derlemesi (Apple Silicon + Intel)");
  run("macOS derlemesi", "npm", ["run", "tauri", "--", "build", "--target", MAC_TARGET], env);

  step("Windows x64 derlemesi (cargo-xwin + NSIS)");
  // tauri:win sets LANG/LC_ALL for makensis and puts llvm/lld on PATH.
  run("Windows derlemesi", "npm", ["run", "tauri:win", "--", "build", "--runner", "cargo-xwin", "--target", WIN_TARGET, "--bundles", "nsis"], env);
  return started;
}

// ---------------------------------------------------------------- checking

/** Decodes a minisign key or signature as Tauri stores it (base64 of the text). */
function minisignLines(stored, what) {
  const lines = Buffer.from(stored.trim(), "base64").toString("utf8").split(/\r?\n/).filter(Boolean);
  if (lines.length < 2 || !lines[0].startsWith("untrusted comment:")) fail(`${what} okunamadı.`);
  return lines;
}

/**
 * Checks a .sig against the updater public key, the way the updater will,
 * so a release signed with the wrong key never goes out. Returns the
 * version bound in the signature, or null for an unbound one.
 */
function verifySignature(file, sigText, pubkeyText) {
  const name = path.basename(file);
  const keyBytes = Buffer.from(minisignLines(pubkeyText, "Açık anahtar")[1], "base64");
  const [, sigLine, trustedLine, globalLine] = minisignLines(sigText, `${name}.sig`);
  const sigBytes = Buffer.from(sigLine ?? "", "base64");
  const globalSig = Buffer.from(globalLine ?? "", "base64");
  if (keyBytes.length !== 42 || sigBytes.length !== 74 || globalSig.length !== 64 || !trustedLine?.startsWith("trusted comment: ")) {
    fail(`${name}.sig biçimi tanınmadı.`);
  }
  if (!keyBytes.subarray(2, 10).equals(sigBytes.subarray(2, 10))) {
    fail(`${name} başka bir anahtarla imzalanmış; tauri.conf.json'daki pubkey ile eşleşmiyor.`);
  }
  const key = createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: keyBytes.subarray(10).toString("base64url") }, format: "jwk" });
  const algorithm = sigBytes.subarray(0, 2).toString("latin1");
  const data = fs.readFileSync(file);
  // "ED" signs the BLAKE2b-512 hash of the file, legacy "Ed" the file itself.
  const message = algorithm === "ED" ? createHash("blake2b512").update(data).digest() : data;
  const signature = sigBytes.subarray(10);
  const trusted = trustedLine.slice("trusted comment: ".length);
  if (!verify(null, message, key, signature) || !verify(null, Buffer.concat([signature, Buffer.from(trusted)]), key, globalSig)) {
    fail(`${name} imzası geçersiz: dosya imzalandıktan sonra değişmiş ya da imza başka dosyanın.`);
  }
  return trusted.split("\t").find((f) => f.startsWith("version:"))?.slice("version:".length) ?? null;
}

/** The version in the Info.plist of the .app inside the updater tarball. */
function tarballVersion(tarball) {
  const list = capture("/usr/bin/tar", ["-tzf", tarball]);
  const entry = list?.split("\n").find((e) => /(^|\/)Kova\.app\/Contents\/Info\.plist$/.test(e));
  if (!entry) fail(`${path.basename(tarball)} içinde Kova.app/Contents/Info.plist yok.`);
  const plist = spawnSync("/usr/bin/tar", ["-xzOf", tarball, entry], { stdio: ["ignore", "pipe", "ignore"] }).stdout;
  const r = spawnSync("/usr/bin/plutil", ["-extract", "CFBundleShortVersionString", "raw", "-o", "-", "-"], { input: plist, encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : null;
}

/** Signature and architectures of the built .app the DMG and tarball hold. */
function checkApp(app) {
  if (spawnSync("/usr/bin/codesign", ["--verify", "--deep", "--strict", app], { stdio: "inherit" }).status !== 0) {
    fail("codesign --verify --deep --strict Kova.app için başarısız oldu.");
  }
  const archs = capture("/usr/bin/lipo", ["-archs", path.join(app, "Contents", "MacOS", "kova")])?.trim().split(/\s+/) ?? [];
  if (!archs.includes("x86_64") || !archs.some((a) => a.startsWith("arm64"))) {
    fail(`Kova.app Universal değil (mimariler: ${archs.join(" ") || "okunamadı"}).`);
  }
  console.log(`Kova.app: imza tamam, mimariler ${archs.join(" ")}`);
}

function isWindowsExe(file) {
  const fd = fs.openSync(file, "r");
  try {
    const head = Buffer.alloc(4096);
    const n = fs.readSync(fd, head, 0, head.length, 0);
    if (n < 0x40 || head.toString("latin1", 0, 2) !== "MZ") return false;
    const pe = head.readUInt32LE(0x3c);
    return pe + 4 <= n && head.toString("latin1", pe, pe + 4) === "PE\0\0";
  } finally {
    fs.closeSync(fd);
  }
}

// ----------------------------------------------------------------- collect

function sha256(file) {
  return createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function size(file) {
  const bytes = fs.statSync(file).size;
  return bytes >= 1e6 ? `${(bytes / 1e6).toFixed(1)} MB` : `${Math.ceil(bytes / 1e3)} KB`;
}

function githubNotes(notes, files) {
  return `${notes}

## Hangi dosyayı indirmeliyim?

| Bilgisayarın | Dosya |
|---|---|
| Mac (Apple Silicon ve Intel, macOS 12 ve sonrası) | \`${files.dmg}\` |
| Windows 10 / 11 (64 bit) | \`${files.setup}\` |

\`.tar.gz\`, \`.sig\` ve \`latest.json\` dosyaları uygulama içi güncelleme içindir, bunları indirmene gerek yok.

İlk açılışta macOS ya da Windows bir uyarı gösterebilir. Ne yapacağın [Kurulum](https://github.com/${REPO}#kurulum) bölümünde yazıyor. Dosyaların SHA-256 özetleri \`SHA256SUMS.txt\` içinde.
`;
}

function collect({ version, pubkey, signedVersionRequired, macosMinimum, notes, builtAfter }) {
  step("Dosyalar toplanıyor");
  const tag = `v${version}`;
  const macBundle = path.join(TARGET_DIR, MAC_TARGET, "release", "bundle");
  const nsisDir = path.join(TARGET_DIR, WIN_TARGET, "release", "bundle", "nsis");
  // Bundler output names: "<productName>_<version>_<arch>" for the DMG and
  // the NSIS setup; the updater tarball is named after the .app only.
  const names = {
    dmg: `Kova_${version}_universal.dmg`,
    tarball: `Kova_${version}_universal.app.tar.gz`,
    setup: `Kova_${version}_x64-setup.exe`,
  };
  const src = {
    app: path.join(macBundle, "macos", "Kova.app"),
    dmg: path.join(macBundle, "dmg", names.dmg),
    tarball: path.join(macBundle, "macos", "Kova.app.tar.gz"),
    setup: path.join(nsisDir, names.setup),
  };
  const needed = [src.dmg, src.tarball, `${src.tarball}.sig`, src.setup, `${src.setup}.sig`];
  const missing = needed.filter((f) => !fs.existsSync(f));
  if (missing.length > 0) {
    // What is there instead, in case the bundler named something differently.
    const dirs = [...new Set(missing.map((f) => path.dirname(f)))];
    const found = dirs.map((d) => `  ${d}: ${fs.existsSync(d) ? fs.readdirSync(d).join(", ") || "boş" : "klasör yok"}`);
    fail(
      `Şu dosyalar yok:\n${missing.map((f) => `  ${f}`).join("\n")}\nBu klasörlerde olanlar:\n${found.join("\n")}\n` +
        "Önce derle (npm run release) ya da yolu kontrol et.",
    );
  }
  if (builtAfter !== null) {
    const stale = needed.filter((f) => fs.statSync(f).mtimeMs < builtAfter);
    if (stale.length > 0) fail(`Bu dosyalar bu derlemede yeniden üretilmemiş:\n${stale.map((f) => `  ${f}`).join("\n")}`);
  } else {
    // Without a build here, show how old the files are so a stale one stands out.
    for (const f of [src.dmg, src.tarball, src.setup]) {
      console.log(`${path.basename(f)}: ${fs.statSync(f).mtime.toLocaleString("tr-TR")} tarihli`);
    }
  }

  if (fs.existsSync(src.app)) checkApp(src.app);
  else if (builtAfter !== null) fail(`${src.app} yok.`);
  else warn("Kova.app bulunamadı, imza ve mimari denetimi atlandı.");

  const macSig = fs.readFileSync(`${src.tarball}.sig`, "utf8").trim();
  const winSig = fs.readFileSync(`${src.setup}.sig`, "utf8").trim();
  for (const [file, sig] of [[src.tarball, macSig], [src.setup, winSig]]) {
    const bound = verifySignature(file, sig, pubkey);
    if (bound === null) {
      const unbound = `${path.basename(file)} imzası sürüme bağlı değil`;
      if (signedVersionRequired) fail(`${unbound}; requireSignedVersion açık olduğu için Kova bu güncellemeyi reddeder.`);
      warn(`${unbound}.`);
    }
    else if (bound !== version) fail(`${path.basename(file)} ${bound} sürümü için imzalanmış, ${version} değil.`);
  }
  console.log("Güncelleme imzaları tauri.conf.json'daki anahtarla doğrulandı.");

  const appVersion = tarballVersion(src.tarball);
  if (appVersion !== version) fail(`Kova.app.tar.gz içindeki uygulama ${appVersion ?? "?"} sürümü, ${version} değil (eski bir derleme kalmış olabilir).`);
  if (!isWindowsExe(src.setup)) fail(`${names.setup} bir Windows programı gibi görünmüyor.`);
  if (spawnSync("/usr/bin/hdiutil", ["verify", "-quiet", src.dmg], { stdio: "inherit" }).status !== 0) {
    fail(`${names.dmg} bozuk görünüyor (hdiutil verify).`);
  }

  const outDir = path.join(RELEASE_DIR, tag);
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });
  const out = (name) => path.join(outDir, name);
  fs.copyFileSync(src.dmg, out(names.dmg));
  fs.copyFileSync(src.tarball, out(names.tarball));
  fs.copyFileSync(`${src.tarball}.sig`, out(`${names.tarball}.sig`));
  fs.copyFileSync(src.setup, out(names.setup));
  fs.copyFileSync(`${src.setup}.sig`, out(`${names.setup}.sig`));

  const url = (name) => `https://github.com/${REPO}/releases/download/${tag}/${name}`;
  const mac = { signature: macSig, url: url(names.tarball) };
  const win = { signature: winSig, url: url(names.setup) };
  // The updater looks up "<os>-<arch>-<installer>" first, then "<os>-<arch>".
  // One Universal tarball serves both Mac architectures.
  const latest = {
    version,
    notes: plainText(notes),
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    macos_minimum: macosMinimum,
    platforms: {
      "darwin-aarch64": mac,
      "darwin-x86_64": mac,
      "darwin-aarch64-app": mac,
      "darwin-x86_64-app": mac,
      "windows-x86_64": win,
      "windows-x86_64-nsis": win,
    },
  };
  fs.writeFileSync(out("latest.json"), `${JSON.stringify(latest, null, 2)}\n`);

  const summed = [names.dmg, names.setup, names.tarball];
  fs.writeFileSync(out("SHA256SUMS.txt"), summed.map((n) => `${sha256(out(n))}  ${n}\n`).join(""));
  fs.writeFileSync(out("notes.md"), githubNotes(notes, names));

  const assets = [names.dmg, names.setup, names.tarball, `${names.tarball}.sig`, `${names.setup}.sig`, "latest.json", "SHA256SUMS.txt"];
  console.log(`\n${outDir}`);
  for (const n of assets) console.log(`  ${n.padEnd(40)} ${size(out(n)).padStart(9)}`);
  return { tag, outDir, assets: assets.map(out), notesFile: out("notes.md") };
}

// ----------------------------------------------------------------- publish

function shellQuote(s) {
  return /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

/** Paths inside the project are printed relative to it, for copy and paste. */
function shown(file) {
  const rel = path.relative(ROOT, file);
  return rel.startsWith("..") || path.isAbsolute(rel) ? file : rel;
}

/** The commit the tag should point at, when the project is a git repo. */
function gitTarget() {
  if (!fs.existsSync(path.join(ROOT, ".git"))) return null;
  const head = capture("git", ["-C", ROOT, "rev-parse", "HEAD"])?.trim();
  if (!head) return null;
  if (capture("git", ["-C", ROOT, "status", "--porcelain"])?.trim()) {
    warn("Commit edilmemiş değişiklikler var. Derleme onları içerir ama etiket son commit'e konur.");
  }
  return head;
}

function ghArgs({ tag, assets, notesFile }, version) {
  const target = gitTarget();
  return [
    "release", "create", tag,
    "--repo", REPO,
    "--title", `Kova ${version}`,
    "--notes-file", shown(notesFile),
    "--latest",
    ...(target ? ["--target", target] : []),
    ...assets.map(shown),
  ];
}

async function confirmPublish(tag) {
  if (!process.stdin.isTTY) fail("--publish yalnızca terminalde, onay yazılarak çalışır.");
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  try {
    const answer = await rl.question(`\n${REPO} için ${tag} herkese açık yayınlanacak. Onaylıyorsan "yayınla" yaz: `);
    return answer.trim().toLocaleLowerCase("tr") === "yayınla";
  } finally {
    rl.close();
  }
}

// -------------------------------------------------------------------- main

async function main() {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log(USAGE);
    return;
  }
  const unknown = args.filter((a) => !["--collect-only", "--publish"].includes(a));
  if (unknown.length > 0) fail(`Bilinmeyen seçenek: ${unknown.join(" ")}\n\n${USAGE}`);
  const collectOnly = args.includes("--collect-only");
  const publish = args.includes("--publish");
  if (process.platform !== "darwin") fail("Bu betik Mac'te çalışır; Windows sürümü de buradan derlenir.");

  const { version, pubkey, signedVersionRequired, macosMinimum } = readConfig();
  checkVersions(version);
  const notes = readNotes(version);
  console.log(`Kova ${version}${collectOnly ? " (yalnızca toplama)" : ""}`);

  const builtAfter = collectOnly ? null : build();
  const release = collect({ version, pubkey, signedVersionRequired, macosMinimum, notes, builtAfter });
  const gh = ghArgs(release, version);

  step("GitHub sürümü");
  console.log("Önce commit'lerini GitHub'a gönder. Sonra proje klasöründe şunu çalıştır:\n");
  console.log(`gh ${gh.map(shellQuote).join(" ")}`);
  if (!publish) return;

  if (!(await confirmPublish(release.tag))) {
    console.log("Yayınlanmadı.");
    return;
  }
  const r = spawnSync("gh", gh, { cwd: ROOT, stdio: "inherit" });
  if (r.status !== 0) fail("gh release create başarısız oldu.");
  console.log(`\nYayında: https://github.com/${REPO}/releases/tag/${release.tag}`);
}

main().catch((e) => {
  console.error(`\nHata: ${e instanceof Failure ? e.message : e?.stack ?? e}`);
  process.exit(1);
});
