import { readFileSync, writeFileSync } from "node:fs";

const moduleTag = '<script type="module" src="/src/main.tsx"></script>';
const source = readFileSync("index.html", "utf8");
if (!source.includes(moduleTag)) {
  throw new Error("index.html no longer contains the expected module script tag");
}

const preview = source
  .replace("<title>知微 Zhiwei</title>", "<title>知微 Zhiwei（预览）</title>")
  .replace(moduleTag, '<script src="/preview-mock.js"></script>\n    ' + moduleTag);
writeFileSync("preview.html", preview);
