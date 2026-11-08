import { readFile, writeFile } from 'node:fs/promises';
const { AMBAR_UPDATER_PUBLIC_KEY: key, AMBAR_SIGNING_KEY: signingKey, AMBAR_REPOSITORY: repo } = process.env;
if (!key?.trim() || !signingKey?.trim() || !/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) {
  throw new Error('AMBAR_UPDATER_PUBLIC_KEY, TAURI_SIGNING_PRIVATE_KEY ve GitHub depo adresi gereklidir.');
}
const file = 'src-tauri/tauri.conf.json';
const config = JSON.parse(await readFile(file, 'utf8'));
config.bundle.createUpdaterArtifacts = true;
config.plugins.updater.pubkey = key.trim();
config.plugins.updater.endpoints = [`https://github.com/${repo}/releases/latest/download/latest.json`];
await writeFile(file, JSON.stringify(config, null, 2) + '\n');
