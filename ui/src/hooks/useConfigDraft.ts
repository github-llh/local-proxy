// 配置草稿状态：编辑副本 + 脏标记，避免各卡片各自维护「已保存」提示。
//
// 除 React 状态外额外保留一份同步副本：保存按钮与 ⌘/Ctrl+S 需要在
// 「失焦提交草稿」的同一 tick 内读到最新值，仅依赖闭包状态会读到旧配置。

import {useCallback, useRef, useState} from "react";
import type {AppConfig} from "../api";

export interface ConfigDraft {
  config: AppConfig | null;
  /** 是否有未保存改动 */
  dirty: boolean;
  /** 就地修改编辑副本并标记为脏 */
  patch: (edit: (config: AppConfig) => void) => void;
  /** 外部加载或保存完成后整体替换 */
  replace: (config: AppConfig) => void;
  /** 只标记为脏（编辑副本之外的草稿，如高级参数的 JSON 文本） */
  markDirty: () => void;
  /** 同步读取最新编辑副本（保存时使用） */
  current: () => AppConfig | null;
}

export function useConfigDraft(): ConfigDraft {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [dirty, setDirty] = useState(false);
  const latest = useRef<AppConfig | null>(null);

  const patch = useCallback((edit: (config: AppConfig) => void) => {
    const previous = latest.current;
    if (!previous) return;
    const next = structuredClone(previous);
    edit(next);
    latest.current = next;
    setConfig(next);
    setDirty(true);
  }, []);

  const replace = useCallback((next: AppConfig) => {
    latest.current = next;
    setConfig(next);
    setDirty(false);
  }, []);

  const markDirty = useCallback(() => setDirty(true), []);

  const current = useCallback(() => latest.current, []);

  return {config, dirty, patch, replace, markDirty, current};
}
