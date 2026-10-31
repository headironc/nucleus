import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { Button } from "@base-ui/react/button";
import { Dialog } from "@base-ui/react/dialog";
import type { Application } from "../protocol/applications";
import { APPLICATION_KINDS } from "./application-display";

export function ReviewDialog({
  application,
  decision,
  pending,
  trigger,
  close,
  submit,
}: {
  application: Application;
  decision: "approve" | "reject";
  pending: boolean;
  trigger: RefObject<HTMLButtonElement | null>;
  close(): void;
  submit(reason: string): void;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const invalid = !!error;
  const reasonField = useRef<HTMLTextAreaElement>(null);
  const errorMessage = useRef<HTMLParagraphElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const rejected = decision === "reject";
  useLayoutEffect(() => {
    if (invalid) errorMessage.current?.scrollIntoView({ block: "nearest" });
  }, [invalid]);
  return (
    <Dialog.Root
      open
      onOpenChange={(open) => {
        if (!open && !pending) close();
      }}
    >
      <Dialog.Portal>
        <Dialog.Backdrop className="application-backdrop" />
        <Dialog.Popup
          className="review-dialog"
          initialFocus={heading}
          finalFocus={trigger}
        >
          <form
            noValidate
            aria-busy={pending}
            onSubmit={(event) => {
              event.preventDefault();
              if (pending) return;
              const invalidReason = !reason.trim()
                ? "请填写驳回理由。"
                : reason.includes("\0")
                  ? "请重新输入驳回理由。"
                  : new TextEncoder().encode(
                        JSON.stringify({ decision: "reject", reason }),
                      ).byteLength > 8192
                    ? "驳回理由过长，请精简后再提交。"
                    : "";
              if (rejected && invalidReason) {
                setError(invalidReason);
                reasonField.current?.focus();
                return;
              }
              submit(rejected ? reason : "");
            }}
          >
            <div className="review-dialog-content">
              <Dialog.Title ref={heading} tabIndex={-1}>
                {pending
                  ? "正在提交审核决定…"
                  : rejected
                    ? "驳回这份申请？"
                    : "批准这份申请？"}
              </Dialog.Title>
              <Dialog.Description>
                {rejected
                  ? "驳回理由会向申请人展示。申请人需重新提交新申请。"
                  : "批准后记录为审核通过，制作和发放另行办理。"}
              </Dialog.Description>
              <div className="review-dialog-summary">
                <strong>{application.snapshot.name}</strong>
                <p>
                  {application.snapshot.printedNumber} ·{" "}
                  {application.snapshot.department.printed}
                </p>
                <p>{APPLICATION_KINDS[application.kind]}工牌申请</p>
              </div>
              {rejected && (
                <div className="review-reason-field">
                  <label htmlFor="review-reason">
                    驳回理由<span>（必填）</span>
                  </label>
                  <textarea
                    id="review-reason"
                    ref={reasonField}
                    value={reason}
                    disabled={pending}
                    aria-required="true"
                    aria-invalid={invalid}
                    aria-describedby={
                      invalid ? "review-reason-error" : undefined
                    }
                    placeholder="说明未通过的原因及需要调整的内容"
                    onChange={(event) => {
                      setReason(event.target.value);
                      setError("");
                    }}
                  />
                  {invalid && (
                    <p ref={errorMessage} id="review-reason-error" role="alert">
                      {error}
                    </p>
                  )}
                </div>
              )}
            </div>
            <div className="review-dialog-actions">
              <Button
                className="secondary"
                type="button"
                disabled={pending}
                onClick={close}
              >
                {rejected ? "暂不驳回" : "暂不批准"}
              </Button>
              <Button
                className={rejected ? "review-danger" : "primary"}
                type="submit"
                disabled={pending}
              >
                {pending ? "正在提交…" : rejected ? "确认驳回" : "确认批准"}
              </Button>
            </div>
          </form>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
