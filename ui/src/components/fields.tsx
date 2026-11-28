// 设置页复用的表单控件：字段外壳、开关、逗号分隔与多行列表输入。

import type {ReactNode} from "react";
import {useListDraft} from "../hooks/useListDraft";

/** 字段外壳：标签 + 控件 + 说明。 */
export function Field({
                        label,
                        hint,
                        children,
                      }: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label>
      {label}
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

/** 开关行（复选框 + 文案 + 可选说明）。 */
export function Toggle({
                         label,
                         hint,
                         checked,
                         onChange,
                         disabled,
                       }: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className="checkbox">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="checkbox-text">
        {label}
        {hint && <span className="field-hint">{hint}</span>}
      </span>
    </label>
  );
}

/** 内联列表输入（表格单元格用），失焦/回车提交。 */
export function ListInput({
                            value,
                            apply,
                            placeholder,
                            disabled,
                            ariaLabel,
                          }: {
  value: string;
  apply: (text: string) => void;
  placeholder?: string;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  const draft = useListDraft(value, apply);
  return (
    <input
      value={draft.text}
      placeholder={placeholder}
      aria-label={ariaLabel}
      spellCheck={false}
      autoComplete="off"
      disabled={disabled}
      onChange={(event) => draft.onChange(event.target.value)}
      onBlur={draft.commit}
      onKeyDown={draft.onKeyDown}
    />
  );
}

/** 逗号分隔的单行输入：失焦/回车才提交，输入中间态不会被裁掉。 */
export function CommaTextField({
                                 label,
                                 hint,
                                 placeholder,
                                 value,
                                 apply,
                                 disabled,
                               }: {
  label: string;
  hint?: string;
  placeholder?: string;
  value: string;
  apply: (text: string) => void;
  disabled?: boolean;
}) {
  return (
    <Field label={label} hint={hint}>
      <ListInput
        value={value}
        apply={apply}
        placeholder={placeholder}
        disabled={disabled}
        ariaLabel={label}
      />
    </Field>
  );
}

/**
 * 数值输入：编辑期间保留文本，失焦/回车才校验并提交。
 *
 * 直接对 number input 做 onChange 提交会让「5 → 5000」的中间态
 * 落到配置里（例如端口 5、超时 5ms），这里统一延迟提交。
 */
export function NumberInput({
                              value,
                              apply,
                              min,
                              max,
                              step,
                              ariaLabel,
                              disabled,
                              placeholder,
                              className,
                            }: {
  value: number;
  apply: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  ariaLabel?: string;
  disabled?: boolean;
  placeholder?: string;
  className?: string;
}) {
  const draft = useListDraft(String(value), (text) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    const parsed = Number(trimmed);
    if (!Number.isFinite(parsed)) return;
    const rounded = Math.trunc(parsed);
    if (min !== undefined && rounded < min) return;
    if (max !== undefined && rounded > max) return;
    apply(rounded);
  });
  return (
    <input
      className={className}
      inputMode="numeric"
      value={draft.text}
      min={min}
      max={max}
      step={step}
      aria-label={ariaLabel}
      placeholder={placeholder}
      disabled={disabled}
      autoComplete="off"
      spellCheck={false}
      onChange={(event) => draft.onChange(event.target.value)}
      onBlur={draft.commit}
      onKeyDown={draft.onKeyDown}
    />
  );
}

/** 多行文本输入（每行一项）：失焦提交，编辑期间保留原始文本。 */
export function LinesField({
                             label,
                             hint,
                             rows = 3,
                             value,
                             apply,
                           }: {
  label: string;
  hint?: string;
  rows?: number;
  value: string;
  apply: (text: string) => void;
}) {
  const draft = useListDraft(value, apply);
  return (
    <Field label={label} hint={hint}>
      <textarea
        rows={rows}
        value={draft.text}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => draft.onChange(event.target.value)}
        onBlur={draft.commit}
      />
    </Field>
  );
}
