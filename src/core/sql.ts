export type SqlKind = "read" | "write";

/**
 * Keywords that make a statement write-capable anywhere after comments and
 * literals have been stripped. Deliberately conservative: a false positive
 * only means "ask for approval" (approval mode) or route to the read-only
 * pool, which fails safely at the database.
 */
const WRITE_KEYWORDS = new Set([
  "INSERT",
  "UPDATE",
  "DELETE",
  "REPLACE",
  "MERGE",
  "UPSERT",
  "CREATE",
  "DROP",
  "ALTER",
  "TRUNCATE",
  "GRANT",
  "REVOKE",
  "CALL",
  "EXEC",
  "EXECUTE",
  "COPY",
  "VACUUM",
  "ATTACH",
  "DETACH",
]);

/** First-token allowlist for statements with no write keyword anywhere. */
const READ_KEYWORDS = new Set([
  "SELECT",
  "SHOW",
  "DESCRIBE",
  "DESC",
  "EXPLAIN",
  "VALUES",
  "PRAGMA",
  "TABLE",
  "WITH",
]);

const DOLLAR_TAG = /^\$[A-Za-z_][A-Za-z0-9_]*\$|^\$\$/;

/**
 * Remove comments and quoted literals while preserving statement structure.
 * This is not a SQL parser; it only needs to be good enough that keywords
 * hidden inside strings/comments do not influence classification, and that
 * keywords in real code are not hidden by quotes.
 */
function stripCommentsAndLiterals(sql: string): string {
  let out = "";
  let i = 0;
  const n = sql.length;

  while (i < n) {
    const ch = sql[i]!;
    const next = i + 1 < n ? sql[i + 1] : "";

    if (ch === "-" && next === "-") {
      i += 2;
      while (i < n && sql[i] !== "\n" && sql[i] !== "\r") i++;
      out += " ";
      continue;
    }

    if (ch === "/" && next === "*") {
      i += 2;
      while (i < n && !(sql[i] === "*" && sql[i + 1] === "/")) i++;
      i = Math.min(i + 2, n);
      out += " ";
      continue;
    }

    if (ch === "'" || ch === '"' || ch === "`") {
      const quote = ch;
      i++;
      while (i < n) {
        if (sql[i] === "\\") {
          i += 2;
          continue;
        }
        if (sql[i] === quote) {
          if (sql[i + 1] === quote) {
            i += 2;
            continue;
          }
          i++;
          break;
        }
        i++;
      }
      out += " ";
      continue;
    }

    if (ch === "$") {
      const match = DOLLAR_TAG.exec(sql.slice(i));
      if (match) {
        const tag = match[0];
        const end = sql.indexOf(tag, i + tag.length);
        if (end === -1) {
          // Unterminated dollar quote: drop the rest, nothing safe to inspect.
          out += " ";
          i = n;
          break;
        }
        i = end + tag.length;
        out += " ";
        continue;
      }
    }

    out += ch;
    i++;
  }

  return out;
}

/**
 * Classify a SQL statement as read or write.
 *
 * - Comments and quoted literals (including dollar-quoted bodies) are ignored.
 * - Any write keyword anywhere in the remaining text wins.
 * - `PRAGMA name = value` is a write; `PRAGMA table_info(x)` is a read.
 * - `EXPLAIN ANALYZE <write>` is a write because the inner keyword is seen.
 * - Unknown first tokens are treated as write (conservative).
 */
export function classify(sql: string): SqlKind {
  const cleaned = stripCommentsAndLiterals(sql).toUpperCase();
  if (cleaned.trim().length === 0) return "read";

  if (/\bPRAGMA\b[^;]*=/.test(cleaned)) return "write";

  const tokens = cleaned.match(/[A-Z_][A-Z0-9_]*/g) ?? [];
  if (tokens.length === 0) return "read";
  if (tokens.some((token) => WRITE_KEYWORDS.has(token))) return "write";

  const first = tokens[0]!;
  return READ_KEYWORDS.has(first) ? "read" : "write";
}