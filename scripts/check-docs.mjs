#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { access, readFile, stat } from "node:fs/promises";
import path from "node:path";

const root = process.cwd();
const listed = execFileSync(
  "git",
  [
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
    "--",
    "*.md",
    "*.html",
  ],
  { cwd: root },
)
  .toString("utf8")
  .split("\0")
  .filter(Boolean)
  .sort();

const errors = [];
const anchorCache = new Map();

const existing = [];
for (const relativeFile of listed) {
  try {
    await access(path.join(root, relativeFile));
    existing.push(relativeFile);
  } catch {
    // `git ls-files --cached` includes tracked paths deleted in the worktree.
  }
}

function withoutFencedCode(source) {
  return source.replace(/^(```|~~~)[\s\S]*?^\1\s*$/gm, "");
}

function targetsFrom(source, extension) {
  const targets = [];
  const body = extension === ".md" ? withoutFencedCode(source) : source;

  if (extension === ".md") {
    const inline = /!?\[[^\]]*\]\((<[^>]+>|[^\s)]+)(?:\s+["'][^)]*["'])?\)/g;
    const reference = /^\s*\[[^\]]+\]:\s*(<[^>]+>|\S+)/gm;
    for (const match of body.matchAll(inline)) targets.push(match[1]);
    for (const match of body.matchAll(reference)) targets.push(match[1]);
  }

  const htmlAttribute = /\b(?:href|src)\s*=\s*["']([^"']+)["']/gi;
  for (const match of body.matchAll(htmlAttribute)) targets.push(match[1]);

  return targets.map((target) =>
    target.startsWith("<") && target.endsWith(">")
      ? target.slice(1, -1)
      : target,
  );
}

function safeDecode(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function markdownSlug(text) {
  return text
    .replace(/<[^>]*>/g, "")
    .replace(/[`*_~]/g, "")
    .toLocaleLowerCase("en-US")
    .trim()
    .replace(/[^\p{Letter}\p{Number}\s-]/gu, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-");
}

async function anchorsFor(file) {
  if (anchorCache.has(file)) return anchorCache.get(file);

  const source = await readFile(file, "utf8");
  const anchors = new Set();
  const extension = path.extname(file).toLowerCase();

  for (const match of source.matchAll(/\b(?:id|name)\s*=\s*["']([^"']+)["']/gi)) {
    anchors.add(match[1]);
  }

  if (extension === ".md") {
    const seen = new Map();
    for (const line of source.split(/\r?\n/)) {
      const heading = line.match(/^#{1,6}\s+(.+?)\s*#*\s*$/);
      if (!heading) continue;
      const base = markdownSlug(heading[1]);
      if (!base) continue;
      const count = seen.get(base) ?? 0;
      seen.set(base, count + 1);
      anchors.add(count === 0 ? base : `${base}-${count}`);
    }
  }

  anchorCache.set(file, anchors);
  return anchors;
}

function isExternal(target) {
  return /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i.test(target);
}

for (const relativeSource of existing) {
  const sourceFile = path.join(root, relativeSource);
  const source = await readFile(sourceFile, "utf8");
  const extension = path.extname(sourceFile).toLowerCase();

  for (const rawTarget of targetsFrom(source, extension)) {
    if (!rawTarget || isExternal(rawTarget)) continue;

    const [rawPath, ...fragmentParts] = rawTarget.split("#");
    const fragment = safeDecode(fragmentParts.join("#"));
    const targetPath = safeDecode(rawPath.split("?")[0]);
    const resolved = targetPath
      ? path.resolve(path.dirname(sourceFile), targetPath)
      : sourceFile;

    try {
      await access(resolved);
    } catch {
      errors.push(`${relativeSource}: missing target ${rawTarget}`);
      continue;
    }

    if (!fragment) continue;

    const targetStat = await stat(resolved);
    if (targetStat.isDirectory()) {
      errors.push(`${relativeSource}: fragment targets a directory ${rawTarget}`);
      continue;
    }

    const targetExtension = path.extname(resolved).toLowerCase();
    if (targetExtension !== ".md" && targetExtension !== ".html") continue;

    const anchors = await anchorsFor(resolved);
    const normalizedFragment = targetExtension === ".md"
      ? fragment.toLocaleLowerCase("en-US")
      : fragment;
    if (!anchors.has(normalizedFragment)) {
      errors.push(`${relativeSource}: missing anchor #${fragment} in ${path.relative(root, resolved)}`);
    }
  }
}

if (errors.length > 0) {
  console.error(`documentation check failed (${errors.length}):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log(`documentation check passed: ${existing.length} Markdown/HTML files`);
