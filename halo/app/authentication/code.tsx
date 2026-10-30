import { useEffect, useRef, useState } from "react";
import { OTPField } from "@base-ui/react/otp-field";

export function CodeInput({
  value,
  onChange,
  disabled,
  invalid,
  incomplete,
}: {
  value: string;
  onChange(value: string): void;
  disabled: boolean;
  invalid: boolean;
  incomplete: boolean;
}) {
  const [error, setError] = useState("");
  const inputs = useRef<(HTMLInputElement | null)[]>([]);
  const reject = useRef(false);
  const composing = useRef(false);
  useEffect(() => {
    setError("");
  }, [value]);
  const message =
    error ||
    (incomplete
      ? "请输入完整的 6 位验证码。"
      : invalid
        ? "验证码不正确，请核对最新短信后重新输入。"
        : "");

  const insert = (raw: string, index: number) => {
    if (disabled) return false;
    const start = raw.length === 6 ? 0 : index;
    if (!/^[0-9]+$/.test(raw) || raw.length > 6 - start) {
      setError(
        "请输入完整的数字验证码，不能包含空格或其他字符，也不能超过 6 位。",
      );
      return false;
    }
    setError("");
    onChange(value.slice(0, start) + raw + value.slice(start + raw.length));
    requestAnimationFrame(() =>
      inputs.current[Math.min(start + raw.length, 5)]?.focus(),
    );
    return true;
  };

  return (
    <div className="code-field">
      <label htmlFor="login-code">短信验证码</label>
      <OTPField.Root
        id="login-code"
        length={6}
        value={value}
        disabled={disabled}
        autoComplete="off"
        onValueInvalid={() => {
          if (disabled) return;
          reject.current = true;
          setError("验证码只能包含 6 位数字，请核对后重新输入。");
          queueMicrotask(() => {
            reject.current = false;
          });
        }}
        onValueChange={(next, details) => {
          if (disabled || reject.current || composing.current) {
            details.cancel();
            return;
          }
          const target = details.event.target;
          if (
            target instanceof HTMLInputElement &&
            (!/^[0-9]*$/.test(target.value) || target.value.length > 6)
          ) {
            details.cancel();
            setError("请输入完整的 6 位数字验证码，不能包含空格或其他字符。");
            return;
          }
          setError("");
          onChange(next);
        }}
        className="code-slots"
      >
        {Array.from({ length: 6 }, (_, index) => (
          <OTPField.Input
            key={index}
            ref={(element) => {
              inputs.current[index] = element;
            }}
            aria-label={index === 0 ? undefined : `第 ${index + 1} 位，共 6 位`}
            aria-describedby="code-hint code-error"
            aria-invalid={!!message}
            inputMode="numeric"
            autoComplete="one-time-code"
            // Reject oversized autofill as a whole instead of native truncation.
            maxLength={undefined}
            onPaste={(event) => {
              event.preventDefault();
              if (!disabled && !composing.current)
                insert(event.clipboardData.getData("text/plain"), index);
            }}
            onChange={(event) => {
              if (disabled) {
                event.currentTarget.value = value[index] ?? "";
                return;
              }
              if (composing.current) return;
              const raw = event.currentTarget.value;
              if (raw && (raw.length > 1 || !/^[0-9]$/.test(raw))) {
                event.preventDefault();
                insert(raw, index);
                event.currentTarget.value = value[index] ?? "";
              }
            }}
            onCompositionStart={() => {
              if (!disabled) composing.current = true;
            }}
            onCompositionEnd={(event) => {
              composing.current = false;
              const raw = event.currentTarget.value;
              // Commit the complete composition once, before Base UI's
              // normalizer can remove characters from the original text.
              reject.current = true;
              if (raw) insert(raw, index);
              event.currentTarget.value = value[index] ?? "";
              queueMicrotask(() => {
                reject.current = false;
              });
            }}
          />
        ))}
      </OTPField.Root>
      <p id="code-hint" className="field-hint">
        请输入最新短信中的 6 位数字验证码，保留开头的 0。
      </p>
      <p id="code-error" className="field-error" role="alert">
        {message}
      </p>
    </div>
  );
}
