import type { SourceConfig } from "../config.ts";
import type { Driver, PoolRole } from "../drivers/types.ts";
import { MssqlDriver } from "../drivers/mssql.ts";
import { MysqlDriver } from "../drivers/mysql.ts";
import { PostgresDriver } from "../drivers/postgres.ts";
import { SqliteDriver } from "../drivers/sqlite.ts";

function createDriver(source: SourceConfig, role: PoolRole): Driver {
  const dsn = role === "ro" ? source.dsnReadonly : source.dsnReadwrite;
  switch (source.driver) {
    case "postgres":
      return new PostgresDriver(dsn, role, source.statementTimeoutMs);
    case "mysql":
      return new MysqlDriver(dsn, role, source.statementTimeoutMs);
    case "sqlite":
      return new SqliteDriver(dsn, role);
    case "mssql":
      return new MssqlDriver(dsn, role, source.statementTimeoutMs);
  }
}

/** Lazy source × role pools. A failed init is retried on the next call. */
export class PoolRegistry {
  private readonly sources: Map<string, SourceConfig>;
  private readonly entries = new Map<string, { driver: Driver; ready: Promise<void> }>();

  constructor(sources: Map<string, SourceConfig>) {
    this.sources = sources;
  }

  async get(sourceId: string, role: PoolRole): Promise<Driver> {
    const source = this.sources.get(sourceId);
    if (!source) throw new Error(`unknown source "${sourceId}"`);

    const key = `${sourceId}:${role}`;
    let entry = this.entries.get(key);
    if (!entry) {
      const driver = createDriver(source, role);
      const ready = driver.init().catch((err: unknown) => {
        this.entries.delete(key);
        throw err;
      });
      entry = { driver, ready };
      this.entries.set(key, entry);
    }
    await entry.ready;
    return entry.driver;
  }

  async closeAll(): Promise<void> {
    const entries = [...this.entries.values()];
    this.entries.clear();
    await Promise.all(entries.map((entry) => entry.driver.close().catch(() => undefined)));
  }
}