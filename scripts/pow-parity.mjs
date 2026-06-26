// Proof-of-work parity check: runs the *shipped* browser worker code from
// src/index.html under Node and confirms it agrees with a reference sha256 over
// the intended preimage, then solves a small challenge with the real solver and
// prints a vector that the Rust test `pow::tests::browser_parity_vector` pins.
//
//   node scripts/pow-parity.mjs
//
// Exits non-zero if the browser hash framing/core diverges from reference sha256.

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

const html = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
const match = html.match(/const workerSource = String\.raw`([\s\S]*?)`;/);
if (!match) {
  console.error("could not locate workerSource in src/index.html");
  process.exit(1);
}

const messages = [];
globalThis.postMessage = (x) => messages.push(x);
// Evaluate the exact worker code that ships to browsers, exporting the two
// entry points we need for the check.
const wrapped =
  match[1] + "\n;globalThis.__sha256Block = sha256Block; globalThis.__onmessage = onmessage;";
(0, eval)(wrapped);

function u32le(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0, 0);
  return b;
}

// Build the worker's 64-byte block and hash it with the shipped sha256Block.
function browserPuzzleHash(salt, address, k, nonce) {
  const block = new Uint8Array(64);
  block.set(salt, 0);
  block.set(address, 16);
  block[36] = k & 0xff; block[37] = (k >>> 8) & 0xff; block[38] = (k >>> 16) & 0xff; block[39] = (k >>> 24) & 0xff;
  block[40] = nonce & 0xff; block[41] = (nonce >>> 8) & 0xff; block[42] = (nonce >>> 16) & 0xff; block[43] = (nonce >>> 24) & 0xff;
  block[44] = 0x80; block[62] = 0x01; block[63] = 0x60;
  const out = new Uint8Array(32);
  globalThis.__sha256Block(block, out);
  return Buffer.from(out).toString("hex");
}

// Reference: plain sha256 over the 44-byte preimage (what Rust's pow::puzzle_hash does).
function refPuzzleHash(salt, address, k, nonce) {
  const pre = Buffer.concat([Buffer.from(salt), Buffer.from(address), u32le(k), u32le(nonce)]);
  return createHash("sha256").update(pre).digest("hex");
}

const salt = Uint8Array.from(Array.from({ length: 16 }, (_, i) => i)); // 00..0f
const address = Uint8Array.from(Array.from({ length: 20 }, (_, i) => i + 0x10)); // 10..23

let ok = true;
for (const [k, n] of [[0, 0], [3, 123456], [7, 99], [255, 4000000]]) {
  const a = browserPuzzleHash(salt, address, k, n);
  const b = refPuzzleHash(salt, address, k, n);
  if (a !== b) {
    ok = false;
    console.error(`MISMATCH k=${k} n=${n}\n  worker=${a}\n  crypto=${b}`);
  }
}
console.log("sha256 framing+core parity:", ok ? "PASS" : "FAIL");

// Solve a small challenge with the real shipped solver.
const bits = 12;
const puzzles = 8;
messages.length = 0;
globalThis.__onmessage({ data: { salt, address, bits, indices: Array.from({ length: puzzles }, (_, i) => i) } });
const nonces = new Array(puzzles).fill(null);
for (const msg of messages) if (msg.t === 2) nonces[msg.index] = msg.nonce;

console.log("--- vector (paste into pow::tests::browser_parity_vector) ---");
console.log("salt    = " + Buffer.from(salt).toString("hex"));
console.log("address = " + Buffer.from(address).toString("hex"));
console.log("bits    = " + bits);
console.log("nonces  = " + JSON.stringify(nonces));
console.log("hash(k=3,n=123456) = " + browserPuzzleHash(salt, address, 3, 123456));

if (!ok) process.exit(1);
