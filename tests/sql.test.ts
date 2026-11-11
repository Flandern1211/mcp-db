import { describe, expect, test } from "bun:test";
import { classify } from "../src/core/sql.ts";

describe("classify", () => {
  test("plain reads", () => {
    expect(classify("SELECT * FROM users")).toBe("read");
    expect(classify("  select 1")).toBe("read");
    expect(classify("VALUES (1)")).toBe("read");
    expect(classify("PRAGMA table_info(users)")).toBe("read");
  });

  test("plain writes", () => {
    expect(classify("INSERT INTO users (name) VALUES ('a')")).toBe("write");
    expect(classify("UPDATE users SET name = 'a'")).toBe("write");
    expect(classify("DELETE FROM users")).toBe("write");
    expect(classify("DROP TABLE users")).toBe("write");
  });

  test("CTE wrapping a write is a write", () => {
    expect(classify("WITH d AS (DELETE FROM users RETURNING *) SELECT * FROM d")).toBe("write");
  });

  test("write keywords inside strings and comments are ignored", () => {
    expect(classify("SELECT 'DROP TABLE users'")).toBe("read");
    expect(classify("-- DELETE FROM users\nSELECT 1")).toBe("read");
    expect(classify("SELECT $$ INSERT INTO t VALUES (1) $$")).toBe("read");
  });

  test("PRAGMA assignment is a write", () => {
    expect(classify("PRAGMA journal_mode = WAL")).toBe("write");
  });

  test("EXPLAIN ANALYZE of a write is a write", () => {
    expect(classify("EXPLAIN ANALYZE UPDATE users SET name = 'a'")).toBe("write");
    expect(classify("EXPLAIN ANALYZE SELECT * FROM users")).toBe("read");
  });

  test("unknown first token is conservative", () => {
    expect(classify("ANALYZE users")).toBe("write");
    expect(classify("")).toBe("read");
  });
});