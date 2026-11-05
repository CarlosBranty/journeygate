// Parses the small TOML subset used by target.toml: comments, [dotted.tables],
// and `key = value` where value is a basic/literal string, integer, boolean or
// a single-line array of those. Throws on anything else so the test fails loudly.
const parseValue = (raw, lineNo) => {
  const v = raw.trim();
  if (v.startsWith('"')) {
    if (!/^"(?:[^"\\]|\\.)*"$/.test(v)) throw new Error(`line ${lineNo}: bad string ${v}`);
    return JSON.parse(v);
  }
  if (v.startsWith("'")) {
    if (!/^'[^']*'$/.test(v)) throw new Error(`line ${lineNo}: bad literal string ${v}`);
    return v.slice(1, -1);
  }
  if (v === 'true' || v === 'false') return v === 'true';
  if (/^[+-]?\d+$/.test(v)) return Number(v);
  if (v.startsWith('[') && v.endsWith(']')) {
    const inner = v.slice(1, -1).trim();
    if (inner === '') return [];
    const items = inner.match(/"(?:[^"\\]|\\.)*"|'[^']*'|[^,\s]+/g) ?? [];
    return items.map((item) => parseValue(item, lineNo));
  }
  throw new Error(`line ${lineNo}: unsupported value ${v}`);
};

const stripComment = (line) => {
  let quote = null;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote) {
      if (ch === '\\' && quote === '"') i++;
      else if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") quote = ch;
    else if (ch === '#') return line.slice(0, i);
  }
  return line;
};

export function parseToml(text) {
  const root = {};
  let table = root;
  text.split(/\r?\n/).forEach((rawLine, index) => {
    const lineNo = index + 1;
    const line = stripComment(rawLine).trim();
    if (line === '') return;
    const header = line.match(/^\[([A-Za-z0-9_.-]+)\]$/);
    if (header) {
      table = root;
      for (const key of header[1].split('.')) {
        table[key] ??= {};
        table = table[key];
      }
      return;
    }
    const kv = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!kv) throw new Error(`line ${lineNo}: cannot parse ${line}`);
    if (kv[1] in table) throw new Error(`line ${lineNo}: duplicate key ${kv[1]}`);
    table[kv[1]] = parseValue(kv[2], lineNo);
  });
  return root;
}
