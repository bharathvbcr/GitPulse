import { describe, expect, it } from "vitest";
import {
  createKeyedSerialQueue,
  createLatestOwnerRegistry,
  fileSaveKey,
} from "./serialSave";

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

describe("createKeyedSerialQueue", () => {
  it("never starts a newer write for the same file before the older write settles", async () => {
    const queue = createKeyedSerialQueue();
    const first = deferred();
    const order: string[] = [];

    const saveA = queue.run("/repo\0src/a.ts", async () => {
      order.push("A:start");
      await first.promise;
      order.push("A:end");
      return "A";
    });
    const saveB = queue.run("/repo\0src/a.ts", async () => {
      order.push("B:start");
      return "B";
    });

    await new Promise<void>((resolve) => queueMicrotask(resolve));
    expect(order).toEqual(["A:start"]);
    first.resolve();
    await expect(Promise.all([saveA, saveB])).resolves.toEqual(["A", "B"]);
    expect(order).toEqual(["A:start", "A:end", "B:start"]);
    expect(queue.pending).toBe(0);
  });

  it("continues after a failed write and does not serialize unrelated files", async () => {
    const queue = createKeyedSerialQueue();
    const first = deferred();
    const order: string[] = [];
    const failed = queue.run("same", async () => {
      order.push("same:first");
      await first.promise;
      throw new Error("disk full");
    });
    const recovered = queue.run("same", async () => {
      order.push("same:second");
      return 2;
    });
    const independent = queue.run("other", async () => {
      order.push("other");
      return 3;
    });

    await expect(independent).resolves.toBe(3);
    expect(order).toEqual(["same:first", "other"]);
    first.resolve();
    await expect(failed).rejects.toThrow("disk full");
    await expect(recovered).resolves.toBe(2);
    expect(order).toEqual(["same:first", "other", "same:second"]);
  });

  it("uses an unambiguous repository-and-path key", () => {
    expect(fileSaveKey("/repo/a", "src/file.ts")).toBe(
      fileSaveKey("/repo/a", "src/file.ts"),
    );
    expect(fileSaveKey("/repo/a", "src/file.ts")).not.toBe(
      fileSaveKey("/repo/b", "src/file.ts"),
    );
    expect(fileSaveKey("/repo", "a\u0000b")).not.toBe(
      fileSaveKey("/repo\u0000a", "b"),
    );
  });

  it("waits for one key or every accepted operation without surfacing prior failures", async () => {
    const queue = createKeyedSerialQueue();
    const first = deferred();
    const second = deferred();
    const events: string[] = [];
    void queue.run("first", async () => {
      await first.promise;
      throw new Error("write failed");
    }).catch(() => undefined);
    void queue.run("second", async () => {
      await second.promise;
    });

    void queue.whenIdle("first").then(() => events.push("first idle"));
    void queue.whenIdle().then(() => events.push("all idle"));
    first.resolve();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(events).toEqual(["first idle"]);
    second.resolve();
    await queue.whenIdle();
    expect(events).toEqual(["first idle", "all idle"]);
  });

  it("keeps waiting when an earlier key is requeued while another target drains", async () => {
    const queue = createKeyedSerialQueue();
    const firstA = deferred();
    const secondA = deferred();
    const b = deferred();
    let targetsIdle = false;
    void queue.run("a", () => firstA.promise);
    void queue.run("b", () => b.promise);
    const waiting = queue.whenIdle(["a", "b"]).then(() => {
      targetsIdle = true;
    });

    firstA.resolve();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    void queue.run("a", () => secondA.promise);
    b.resolve();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(targetsIdle).toBe(false);

    secondA.resolve();
    await waiting;
    expect(targetsIdle).toBe(true);
  });
});

describe("createLatestOwnerRegistry", () => {
  it("lets only the newest instance for a repository apply local state", () => {
    const owners = createLatestOwnerRegistry<string>();
    const oldViewer = owners.claim("/repo", "old");
    const otherRepo = owners.claim("/other", "other");
    expect(oldViewer.isCurrent()).toBe(true);
    expect(otherRepo.isCurrent()).toBe(true);
    expect(owners.current("/repo")).toBe("old");

    const newViewer = owners.claim("/repo", "new");
    expect(oldViewer.isCurrent()).toBe(false);
    expect(newViewer.isCurrent()).toBe(true);
    expect(otherRepo.isCurrent()).toBe(true);
    expect(owners.current("/repo")).toBe("new");

    // A destroyed stale instance must not release the newer owner's claim.
    oldViewer.release();
    expect(newViewer.isCurrent()).toBe(true);
    newViewer.release();
    expect(newViewer.isCurrent()).toBe(false);
    expect(owners.current("/repo")).toBeUndefined();
  });
});
