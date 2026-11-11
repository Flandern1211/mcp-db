#!/usr/bin/env bun
/**
 * Real-database integration entry. Not picked up by `bun test`.
 *
 * Required env per driver (set only the ones you want to run):
 *   MCPDB_TEST_PG_RW / MCPDB_TEST_PG_RO
 *   MCPDB_TEST_MYSQL_RW / MCPDB_TEST_MYSQL_RO
 *   MCPDB_TEST_MSSQL_RW / MCPDB_TEST_MSSQL_RO
 *
 * The RW account needs DDL/DML; the RO account must be read-only. The script
 * creates and drops one table named mcp_it_test.
 */
import type { Driver, DriverName, PoolRole } from "../src/drivers/types.ts";
import { errorMessage } from "../src/drivers/types.ts";
import { MssqlDriver } from "../src/drivers/mssql.ts";
import { MysqlDriver } from "../src/drivers/mysql.ts";
import { PostgresDriver } from "../src/drivers/postgres.ts";

const TABLE = "mcp_it_test";

interface Case {
  name: DriverName;
  rwDsn: string;
  roDsn: string;
  make: (dsn: string, role: PoolRole, timeoutMs?: number) => Driver;
  insertSql: string;
  selectByNameSql: string;
  sleepSql: string;
}

function casesFromEnv(): Case[] {
  const cases: Case[] = [];
  const pgRw = process.env.MCPDB_TEST_PG_RW;
  const pgRo = process.env.MCPDB_TEST_PG_RO;
  if (pgRw && pgRo) {
    cases.push({
      name: "postgres",
      rwDsn: pgRw,
      roDsn: pgRo,
      make: (dsn, role, timeoutMs) => new PostgresDriver(dsn, role, timeoutMs),
      insertSql: `INSERT INTO ${TABLE} (id, name) VALUES ($1, $2)`,
      selectByNameSql: `SELECT name FROM ${TABLE} WHERE name = $1`,
      sleepSql: "SELECT pg_sleep(2)",
    });
  }

  const myRw = process.env.MCPDB_TEST_MYSQL_RW;
  const myRo = process.env.MCPDB_TEST_MYSQL_RO;
  if (myRw && myRo) {
    cases.push({
      name: "mysql",
      rwDsn: myRw,
      roDsn: myRo,
      make: (dsn, role, timeoutMs) => new MysqlDriver(dsn, role, timeoutMs),
      insertSql: `INSERT INTO ${TABLE} (id, name) VALUES (?, ?)`,
      selectByNameSql: `SELECT name FROM ${TABLE} WHERE name = ?`,
      sleepSql: "SELECT SLEEP(2)",
    });
  }

  const msRw = process.env.MCPDB_TEST_MSSQL_RW;
  const msRo = process.env.MCPDB_TEST_MSSQL_RO;
  if (msRw && msRo) {
    cases.push({
      name: "mssql",
      rwDsn: msRw,
      roDsn: msRo,
      make: (dsn, role, timeoutMs) => new MssqlDriver(dsn, role, timeoutMs),
      insertSql: `INSERT INTO ${TABLE} (id, name) VALUES (@p1, @p2)`,
      selectByNameSql: `SELECT name FROM ${TABLE} WHERE name = @p1`,
      sleepSql: "WAITFOR DELAY '00:00:02'",
    });
  }

  return cases;
}

async function runCase(testCase: Case): Promise<string> {
  const rw = testCase.make(testCase.rwDsn, "rw", 5_000);
  const ro = testCase.make(testCase.roDsn, "ro", 5_000);
  const slow = testCase.make(testCase.rwDsn, "rw", 300);
  const notes: string[] = [];

  try {
    await rw.init();
    await ro.init();
    await slow.init();

    await rw.execute(`DROP TABLE IF EXISTS ${TABLE}`, []);
    await rw.execute(`CREATE TABLE ${TABLE} (id INT PRIMARY KEY, name VARCHAR(100))`, []);
    notes.push("ddl");

    const payload = "x'); DROP TABLE mcp_it_test; --";
    await rw.execute(testCase.insertSql, [1, payload]);
    notes.push("insert");

    const found = await ro.execute(testCase.selectByNameSql, [payload]);
    if (found.rowCount !== 1) throw new Error(`param binding: expected 1 row, got ${found.rowCount}`);
    notes.push("params");

    const tables = await ro.listTables();
    if (!tables.some((table) => table.name === TABLE)) throw new Error("listTables missed the test table");
    notes.push("list");

    const description = await rw.describeTable(TABLE);
    const idColumn = description.columns.find((column) => column.name === "id");
    if (!idColumn?.primaryKey) throw new Error("describeTable missed the primary key");
    if (!description.columns.some((column) => column.name === "name")) {
      throw new Error("describeTable missed the name column");
    }
    notes.push("describe");

    let roWriteFailed = false;
    try {
      await ro.execute(testCase.insertSql, [2, "ro"]);
    } catch {
      roWriteFailed = true;
    }
    if (!roWriteFailed) throw new Error("read-only account was able to write");
    notes.push("ro-write-rejected");

    const started = performance.now();
    let timedOut = false;
    try {
      await slow.execute(testCase.sleepSql, []);
    } catch {
      timedOut = true;
    }
    const elapsed = Math.round(performance.now() - started);
    if (!timedOut) throw new Error("statement timeout did not fire");
    if (elapsed > 5_000) throw new Error(`timeout took ${elapsed}ms`);
    notes.push(`timeout ${elapsed}ms`);

    return `${testCase.name}: ok (${notes.join(", ")})`;
  } finally {
    await rw.execute(`DROP TABLE IF EXISTS ${TABLE}`, []).catch(() => undefined);
    await rw.close().catch(() => undefined);
    await ro.close().catch(() => undefined);
    await slow.close().catch(() => undefined);
  }
}

const cases = casesFromEnv();
if (cases.length === 0) {
  console.error("No real-database DSNs configured. Set MCPDB_TEST_<DRIVER>_RW / _RO.");
  process.exit(1);
}

let failed = 0;
for (const testCase of cases) {
  try {
    console.log(await runCase(testCase));
  } catch (err) {
    failed++;
    console.error(`${testCase.name}: FAILED - ${errorMessage(err)}`);
  }
}

process.exit(failed === 0 ? 0 : 1);