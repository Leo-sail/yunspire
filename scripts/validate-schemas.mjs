import { readFile, readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

const projectRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const schemaDirectories = [
  resolve(projectRoot, "docs/schemas"),
  resolve(projectRoot, "skills/beautify-markdown"),
];

const schemaPaths = (await Promise.all(schemaDirectories.map(async (directory) =>
  (await readdir(directory))
    .filter((name) => name.endsWith(".schema.json"))
    .map((name) => resolve(directory, name))))).flat();

const ajv = new Ajv2020({ allErrors: true, strict: true });
addFormats(ajv);

for (const schemaPath of schemaPaths) {
  const schema = JSON.parse(await readFile(schemaPath, "utf8"));
  ajv.compile(schema);
}

console.log(`SCHEMA_OK ${schemaPaths.length}`);
