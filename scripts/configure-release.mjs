import { readFile, writeFile } from 'node:fs/promises';
const {
  AMBAR_UPDATER_PUBLIC_KEY: key,
  AMBAR_SIGNING_KEY: signingKey,
  AMBAR_REPOSITORY: repo,
  AMBAR_RELEASE_VERSION: version,
} = process.env;
if (!key?.trim() || !signingKey?.trim() || !/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) {
  throw new Error('AMBAR_UPDATER_PUBLIC_KEY, TAURI_SIGNING_PRIVATE_KEY ve GitHub depo adresi gereklidir.');
}
if (!/^\d+\.\d+\.\d+$/.test(version ?? '')) {
  throw new Error('AMBAR_RELEASE_VERSION üç sayılı bir sürüm olmalıdır.');
}
const file = 'src-tauri/tauri.conf.json';
const config = JSON.parse(await readFile(file, 'utf8'));
config.version = version;
config.bundle.createUpdaterArtifacts = true;
config.plugins.updater.pubkey = key.trim();
config.plugins.updater.endpoints = [`https://github.com/${repo}/releases/latest/download/latest.json`];
await writeFile(file, JSON.stringify(config, null, 2) + '\n');

for (const path of ['package.json', 'package-lock.json']) {
  const packageData = JSON.parse(await readFile(path, 'utf8'));
  packageData.version = version;
  if (path === 'package-lock.json') packageData.packages[''].version = version;
  await writeFile(path, JSON.stringify(packageData, null, 2) + '\n');
}

const cargoFile = 'src-tauri/Cargo.toml';
const cargo = await readFile(cargoFile, 'utf8');
const updatedCargo = cargo.replace(/^(\[package\][\s\S]*?^version\s*=\s*)"[^"]+"/m, `$1"${version}"`);
if (updatedCargo === cargo && !cargo.includes(`version = "${version}"`)) throw new Error('Cargo.toml sürümü değiştirilemedi.');
await writeFile(cargoFile, updatedCargo);

const lockFile = 'src-tauri/Cargo.lock';
const lock = await readFile(lockFile, 'utf8');
const updatedLock = lock.replace(/(\[\[package\]\]\r?\nname = "ambar"\r?\nversion = )"[^"]+"/, `$1"${version}"`);
if (updatedLock === lock && !/name = "ambar"\r?\nversion = "[^\"]+"/.test(lock)) throw new Error('Cargo.lock içindeki Ambar paketi bulunamadı.');
await writeFile(lockFile, updatedLock);
