import { describe, expect, it } from "vitest";
import {
  FIELD_CHAR_PX,
  FIELD_CHROME_PX,
  FIELD_KEYCAP_MIN_WIDTH,
  FIELD_KEYCAP_PX,
  paletteFieldCopy,
} from "./fieldCopy";

const LONG = "Search this repository: commands, / files, @ branches, # commits";

describe("paletteFieldCopy", () => {
  it("says the whole hint before the field has been measured", () => {
    expect(paletteFieldCopy({ subject: "this repository", width: null })).toEqual({
      placeholder: LONG,
      label: "Search this repository",
      showKeycap: true,
    });
  });

  it("names the repository on the host-wide surfaces", () => {
    const copy = paletteFieldCopy({ subject: "gitpulse", width: null });
    expect(copy.placeholder).toBe("Search gitpulse: commands, / files, @ branches, # commits");
    expect(copy.label).toBe("Search gitpulse");
  });

  it("falls back to commands and repositories with no repository open", () => {
    const copy = paletteFieldCopy({ subject: null, width: null });
    expect(copy.placeholder).toBe("Search commands, % repositories");
    expect(copy.label).toBe("Search commands");
  });

  it("keeps the long hint where it fits whole", () => {
    const width = FIELD_CHROME_PX + FIELD_KEYCAP_PX + LONG.length * FIELD_CHAR_PX + 1;
    expect(paletteFieldCopy({ subject: "this repository", width }).placeholder).toBe(LONG);
  });

  it("drops to the subject alone, then to one word, never to a cut sentence", () => {
    const subject = "gitpulse";
    const mid = FIELD_CHROME_PX + FIELD_KEYCAP_PX + `Search ${subject}`.length * FIELD_CHAR_PX + 1;
    expect(paletteFieldCopy({ subject, width: mid }).placeholder).toBe("Search gitpulse");
    // Just wide enough for the hint, so the hint's room leaves no phrase whole.
    expect(paletteFieldCopy({ subject, width: FIELD_KEYCAP_MIN_WIDTH }).placeholder).toBe("Search");
    // The 96px floor's content box (the pill less its border and padding).
    expect(paletteFieldCopy({ subject, width: 82 }).placeholder).toBe("Search");
    expect(paletteFieldCopy({ subject: null, width: 0 }).placeholder).toBe("Search");
  });

  it("drops the shortcut hint below its threshold and keeps it from there up", () => {
    expect(paletteFieldCopy({ subject: "x", width: FIELD_KEYCAP_MIN_WIDTH - 1 }).showKeycap).toBe(false);
    expect(paletteFieldCopy({ subject: "x", width: FIELD_KEYCAP_MIN_WIDTH }).showKeycap).toBe(true);
  });

  it("gives the room the hint no longer takes back to the text", () => {
    // The same text that does not fit beside the hint fits once it is gone.
    const text = "Search gitpulse";
    const width = FIELD_KEYCAP_MIN_WIDTH - 1;
    expect(FIELD_KEYCAP_MIN_WIDTH - FIELD_CHROME_PX - FIELD_KEYCAP_PX).toBeLessThan(text.length * FIELD_CHAR_PX);
    expect(width - FIELD_CHROME_PX).toBeGreaterThanOrEqual(text.length * FIELD_CHAR_PX);
    expect(paletteFieldCopy({ subject: "gitpulse", width }).placeholder).toBe(text);
  });
});
