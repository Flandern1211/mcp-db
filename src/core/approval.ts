import type { ApprovalChannel, ApprovalRecord, Store } from "./store.ts";

export interface ApprovalRequest {
  agent: string;
  source: string;
  tool: string;
  sql: string;
  paramsJson: string | null;
}

export interface Decision {
  approved: boolean;
  channel: ApprovalChannel;
  reason?: string;
}

/**
 * Two approval channels share one approval table:
 * - elicitation: decided inside the tool call, no queue;
 * - cli: a pending row the operator decides through the admin API.
 */
export class Approvals {
  private readonly store: Store;
  private readonly timeoutMs: number;
  private readonly waiters = new Map<number, (approved: boolean) => void>();

  constructor(store: Store, timeoutMs: number) {
    this.store = store;
    this.timeoutMs = timeoutMs;
    // Pending rows left over from a previous process can never be decided.
    this.store.expirePending();
  }

  listPending(): ApprovalRecord[] {
    return this.store.listPending();
  }

  get(id: number): ApprovalRecord | null {
    return this.store.getApproval(id);
  }

  /** CLI fallback: block the tool call until an operator decides or it times out. */
  requestViaCli(request: ApprovalRequest): Promise<Decision> {
    const createdAt = Date.now();
    const expiresAt = createdAt + this.timeoutMs;
    const id = this.store.insertApproval({
      ...request,
      status: "pending",
      channel: "cli",
      createdAt,
      expiresAt,
      decidedAt: null,
    });

    return new Promise<Decision>((resolve) => {
      const timer = setTimeout(() => {
        this.waiters.delete(id);
        this.store.decide(id, "expired");
        resolve({ approved: false, channel: "cli", reason: "approval timed out" });
      }, this.timeoutMs);

      this.waiters.set(id, (approved) => {
        clearTimeout(timer);
        resolve({
          approved,
          channel: "cli",
          reason: approved ? undefined : "denied by operator",
        });
      });
    });
  }

  /** Record a decision made in the elicitation dialog. */
  recordElicitation(request: ApprovalRequest, approved: boolean, reason?: string): Decision {
    const now = Date.now();
    this.store.insertApproval({
      ...request,
      status: approved ? "approved" : "denied",
      channel: "elicitation",
      createdAt: now,
      expiresAt: now,
      decidedAt: now,
    });
    return {
      approved,
      channel: "elicitation",
      reason: approved ? undefined : (reason ?? "denied by user"),
    };
  }

  decide(id: number, approved: boolean): "approved" | "denied" | "missing" {
    const status = approved ? "approved" : "denied";
    if (!this.store.decide(id, status)) return "missing";
    const waiter = this.waiters.get(id);
    this.waiters.delete(id);
    waiter?.(approved);
    return status;
  }

  /** Resolve all blocked calls as denied when the server is shutting down. */
  close(): void {
    for (const [id, waiter] of this.waiters) {
      this.store.decide(id, "expired");
      waiter(false);
    }
    this.waiters.clear();
  }
}