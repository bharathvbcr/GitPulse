import { useMemo } from "react";
import { StyleSheet, Text, View } from "react-native";
import { FlashList } from "@shopify/flash-list";
import {
  parseUnifiedDiffToFlatLines,
  type AnnotatedDiffLine,
} from "../diff/mobileDiff";
import type { DiffHunkFile } from "../protocol/types";
import { colors, radii, spacing } from "../theme/tokens";

export function DiffHunkList({ files }: { files: readonly DiffHunkFile[] }) {
  const rows = useMemo<AnnotatedDiffLine[]>(() => {
    const flattened: AnnotatedDiffLine[] = [];
    for (const file of files) {
      flattened.push({
        id: `file-header:${file.path}`,
        type: "meta",
        content: `${file.path}  (+${file.additions} / -${file.deletions})`,
      });
      flattened.push(...parseUnifiedDiffToFlatLines(file.path, file.unified_diff));
    }
    return flattened;
  }, [files]);

  if (rows.length === 0) {
    return (
      <View style={styles.empty}>
        <Text style={styles.emptyText}>No uncommitted diff hunks in this session.</Text>
      </View>
    );
  }

  return (
    <View style={styles.container}>
      <FlashList
        data={rows}
        keyExtractor={(item: AnnotatedDiffLine) => item.id}
        renderItem={({ item }: { item: AnnotatedDiffLine }) => <DiffLineRow line={item} />}
      />
    </View>
  );
}

function DiffLineRow({ line }: { line: AnnotatedDiffLine }) {
  const rowBg =
    line.type === "add"
      ? colors.diffAddBg
      : line.type === "del"
        ? colors.diffDelBg
        : line.type === "hdr"
          ? colors.diffHdrBg
          : "transparent";

  const prefix = line.type === "add" ? "+" : line.type === "del" ? "-" : " ";

  return (
    <View style={[styles.row, { backgroundColor: rowBg }]}>
      <Text style={styles.gutter}>
        {line.oldNo ?? ""} {line.newNo ?? ""}
      </Text>
      <Text style={styles.code} numberOfLines={0}>
        {line.type !== "meta" && line.type !== "hdr" ? prefix : ""}
        {line.segments
          ? line.segments.map((seg, idx) => (
              <Text
                key={idx}
                style={
                  seg.kind === "Added"
                    ? styles.wordAdd
                    : seg.kind === "Removed"
                      ? styles.wordDel
                      : undefined
                }
              >
                {seg.text}
              </Text>
            ))
          : line.content}
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    height: 360,
    backgroundColor: colors.bg,
    borderRadius: radii.md,
    borderWidth: 1,
    borderColor: colors.border,
    overflow: "hidden",
  },
  empty: {
    padding: spacing.lg,
    backgroundColor: colors.surface,
    borderRadius: radii.md,
    borderWidth: 1,
    borderColor: colors.border,
  },
  emptyText: {
    color: colors.textMuted,
    fontSize: 13,
  },
  row: {
    flexDirection: "row",
    paddingHorizontal: spacing.sm,
    paddingVertical: 2,
  },
  gutter: {
    width: 56,
    fontFamily: "Courier",
    fontSize: 11,
    color: colors.textDim,
  },
  code: {
    flex: 1,
    fontFamily: "Courier",
    fontSize: 12,
    color: colors.text,
  },
  wordAdd: {
    backgroundColor: colors.diffAddWordBg,
  },
  wordDel: {
    backgroundColor: colors.diffDelWordBg,
  },
});
