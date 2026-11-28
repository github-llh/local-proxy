// 列表型输入的编辑草稿：raw 文本在失焦/回车时才解析回配置，
// 避免「80,4」这类中间态被立即规范化导致字符被吞。

import type {KeyboardEvent} from "react";
import {useCallback, useState} from "react";

export interface ListDraft {
  text: string;
  onChange: (text: string) => void;
  /** 失焦或回车时提交（文本未变化则不触发） */
  commit: () => void;
  onKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
}

/**
 * `value` 为外部格式化后的规范文本（如 `80, 443`）；
 * 编辑期间展示草稿，提交后回到 `value`。
 */
export function useListDraft(value: string, apply: (text: string) => void): ListDraft {
  const [draft, setDraft] = useState<string | null>(null);

  const commit = useCallback(() => {
    if (draft === null) return;
    if (draft !== value) apply(draft);
    setDraft(null);
  }, [apply, draft, value]);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Enter") {
        event.currentTarget.blur();
      }
    },
    []
  );

  return {text: draft ?? value, onChange: setDraft, commit, onKeyDown};
}
