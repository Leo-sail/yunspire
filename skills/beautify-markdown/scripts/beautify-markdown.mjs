import process from "node:process";

const input = await new Promise((resolve, reject) => {
  let value = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => { value += chunk; });
  process.stdin.on("end", () => resolve(value));
  process.stdin.on("error", reject);
});

const protectedBlocks = [];
const protect = (value) => {
  const token = `YUNSPIREPROTECTED${protectedBlocks.length}TOKEN`;
  protectedBlocks.push(value);
  return token;
};

let markdown = String(input).replace(/\r\n?/g, "\n");
markdown = markdown.replace(/^---\n[\s\S]*?\n---(?=\n|$)/, protect);
markdown = markdown.replace(/```[\s\S]*?```|~~~[\s\S]*?~~~/g, protect);
markdown = markdown.replace(/`[^`\n]+`|\$\$[\s\S]*?\$\$|\$[^$\n]+\$/g, protect);
markdown = markdown.replace(/!\[\[[^\]]+\]\]|\[\[[^\]]+\]\]|!\[[^\]]*\]\([^\n)]+\)|\[[^\]]+\]\([^\n)]+\)|https?:\/\/[^\s)]+/g, protect);

markdown = markdown
  .split("\n")
  .map((line) => line.replace(/[ \t]+$/g, "").replace(/^(#{1,6})\s*/, "$1 ").replace(/^([*-])\s*/, "$1 "))
  .join("\n")
  .replace(/\n{3,}/g, "\n\n")
  .replace(/([^\n])\n(#{1,6} )/g, "$1\n\n$2")
  .replace(/(#{1,6} [^\n]+)\n(?!\n)/g, "$1\n\n")
  .replace(/([^\n])\n(> )/g, "$1\n\n$2")
  .replace(/(^> .*$(?:\n^> .*$)*)\n(?!\n)/gm, "$1\n\n");

markdown = markdown
  .replace(/([\u3400-\u9fff])([A-Za-z0-9])/g, "$1 $2")
  .replace(/([A-Za-z0-9])([\u3400-\u9fff])/g, "$1 $2");

markdown = markdown.replace(/YUNSPIREPROTECTED(\d+)TOKEN/g, (_, index) => protectedBlocks[Number(index)]);
process.stdout.write(`${markdown.trim()}\n`);
