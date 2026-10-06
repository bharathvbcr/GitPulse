import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { consumeTaskOpen, isTaskId, openTaskForRun, requestTaskOpen, taskOpenRequest } = await import("./taskOpen");
const { interfaceStore } = await import("../stores/interfaceStore");

afterEach(() => {
  const pending = get(taskOpenRequest);
  if (pending) consumeTaskOpen(pending);
  interfaceStore.setGlobalSurface("repository");
});

describe("opening a task from a terminal session", () => {
  it("queues the task and shows the Tasks surface", () => {
    requestTaskOpen("task-1");
    expect(get(taskOpenRequest)).toBe("task-1");
    expect(get(interfaceStore).globalSurface).toBe("tasks");
  });

  it("is taken exactly once, and only by the id it names", () => {
    requestTaskOpen("task-1");
    expect(consumeTaskOpen("task-2")).toBe(false);
    expect(get(taskOpenRequest)).toBe("task-1");
    expect(consumeTaskOpen("task-1")).toBe(true);
    expect(consumeTaskOpen("task-1")).toBe(false);
    expect(get(taskOpenRequest)).toBeNull();
  });

  it("opens the same task again after the first request was taken", () => {
    requestTaskOpen("task-1");
    consumeTaskOpen("task-1");
    requestTaskOpen("task-1");
    expect(get(taskOpenRequest)).toBe("task-1");
  });

  it("the latest request wins over one not yet taken", () => {
    requestTaskOpen("task-1");
    requestTaskOpen("task-2");
    expect(get(taskOpenRequest)).toBe("task-2");
    expect(consumeTaskOpen("task-1")).toBe(false);
  });

  it("refuses an id the store could never have issued, and queues nothing", () => {
    for (const bad of ["", "a b", "../x", "x".repeat(129), "t\0", "täsk", 7, null, undefined]) {
      expect(isTaskId(bad)).toBe(false);
      if (typeof bad === "string") expect(() => requestTaskOpen(bad)).toThrow(/not valid/);
    }
    expect(get(taskOpenRequest)).toBeNull();
    expect(get(interfaceStore).globalSurface).toBe("repository");
    expect(isTaskId("x".repeat(128))).toBe(true);
  });

  it("finds the task from the run, which is the only record that names it", async () => {
    const read = vi.fn(async (id: string) => ({ id, task_id: "task-9" }));
    await openTaskForRun("run-3", read);
    expect(read).toHaveBeenCalledWith("run-3");
    expect(get(taskOpenRequest)).toBe("task-9");
  });

  it("a run that cannot be read, or answers for another run, opens nothing", async () => {
    await expect(openTaskForRun("run-3", async () => { throw new Error("not found"); })).rejects.toThrow("not found");
    await expect(openTaskForRun("run-3", async () => ({ id: "run-4", task_id: "task-9" }))).rejects.toThrow(/could not be read/);
    await expect(openTaskForRun("run-3", async () => ({ id: "run-3", task_id: "bad id" }))).rejects.toThrow(/not valid/);
    expect(get(taskOpenRequest)).toBeNull();
    expect(get(interfaceStore).globalSurface).toBe("repository");
  });
});
