export type DriverName = "postgres" | "mysql" | "sqlite" | "mssql";

export type PoolRole = "ro" | "rw";

export interface TableRef {
  schema: string;
  name: string;
  type: string;
}

export interface TableColumn {
  name: string;
  type: string;
  nullable: boolean;
  default: string | null;
  primaryKey: boolean;
}

export interface TableIndex {
  name: string;
  unique: boolean;
  columns: string[];
}

export interface TableForeignKey {
  name: string;
  columns: string[];
  referencedTable: string;
  referencedColumns: string[];
}

export interface TableDescription {
  schema: string;
  table: string;
  columns: TableColumn[];
  indexes: TableIndex[];
  foreignKeys: TableForeignKey[];
}

export interface QueryResult {
  columns: string[];
  rows: Record<string, unknown>[];
  rowCount: number;
  durationMs: number;
}

/**
 * A single connection pool bound to one role ("ro" or "rw").
 * Drivers never decide routing themselves: the registry hands out the pool,
 * and the tool layer picks the role before calling the driver.
 */
export interface Driver {
  readonly driver: DriverName;
  readonly role: PoolRole;

  /** Open the underlying pool / handle. May throw. */
  init(): Promise<void>;

  /** Cheap liveness check. */
  ping(): Promise<boolean>;

  listTables(schema?: string): Promise<TableRef[]>;

  describeTable(table: string, schema?: string): Promise<TableDescription>;

  /** Returns a human-readable query plan. */
  explain(sql: string, params: unknown[]): Promise<string>;

  execute(sql: string, params: unknown[]): Promise<QueryResult>;

  close(): Promise<void>;
}

export function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}