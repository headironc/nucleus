import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { Button } from "@base-ui/react/button";
import { Dialog } from "@base-ui/react/dialog";
import type { ApplicationDetail } from "../protocol/applications";
import {
  APPLICATION_KINDS,
  APPLICATION_STATUSES,
  applicationDate,
} from "./application-display";

export function CancellationDialog({
  application,
  mode,
  pending,
  trigger,
  close,
  submit,
}: {
  application: ApplicationDetail["application"];
  mode: "self" | "manager";
  pending: boolean;
  trigger: RefObject<HTMLButtonElement | null>;
  close(): void;
  submit(reason: string): void;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const keep = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const errorMessage = useRef<HTMLParagraphElement>(null);
  const manager = mode === "manager";
  const cancellable =
    application.status === "submitted" || application.status === "approved";
  useLayoutEffect(() => {
    if (error) errorMessage.current?.scrollIntoView({ block: "nearest" });
  }, [error]);
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
          initialFocus={keep}
          finalFocus={trigger}
        >
          <form
            noValidate
            aria-busy={pending}
            onSubmit={(event) => {
              event.preventDefault();
              if (pending || !cancellable) return;
              if (manager && (!reason.trim() || reason.includes("\0"))) {
                setError(
                  reason.trim() ? "请重新输入取消说明。" : "请填写取消说明。",
                );
                field.current?.focus();
                return;
              }
              if (
                manager &&
                new TextEncoder().encode(
                  JSON.stringify({
                    mode,
                    expectedStatus: application.status,
                    reason,
                  }),
                ).byteLength > 8192
              ) {
                setError("取消说明过长，请精简后再提交。");
                field.current?.focus();
                return;
              }
              submit(manager ? reason : "");
            }}
          >
            <div className="review-dialog-content">
              <Dialog.Title>
                {pending ? "正在取消申请…" : "取消申请？"}
              </Dialog.Title>
              <Dialog.Description>
                取消后无法恢复。
                {manager
                  ? "申请人如需办理，须重新申请。"
                  : "需要办理时，请重新申请。"}
              </Dialog.Description>
              <div className="review-dialog-summary">
                {"snapshot" in application ? (
                  <>
                    <strong>{application.snapshot.name}</strong>
                    <p>
                      {application.snapshot.printedNumber} ·{" "}
                      {application.snapshot.department.printed}
                    </p>
                    <p>
                      {APPLICATION_KINDS[application.kind]}工牌申请 ·{" "}
                      {APPLICATION_STATUSES[application.status]}
                    </p>
                  </>
                ) : (
                  <dl className="cancellation-summary">
                    <div>
                      <dt>申请编号</dt>
                      <dd>{application.id}</dd>
                    </div>
                    <div>
                      <dt>申请类型</dt>
                      <dd>{APPLICATION_KINDS[application.kind]}</dd>
                    </div>
                    <div>
                      <dt>申请状态</dt>
                      <dd>{APPLICATION_STATUSES[application.status]}</dd>
                    </div>
                    <div>
                      <dt>提交时间</dt>
                      <dd>{applicationDate(application.submittedAt)}</dd>
                    </div>
                  </dl>
                )}
              </div>
              {manager && (
                <div className="review-reason-field">
                  <label htmlFor="cancellation-reason">
                    取消说明<span>（必填）</span>
                  </label>
                  <textarea
                    id="cancellation-reason"
                    ref={field}
                    value={reason}
                    disabled={pending}
                    aria-required="true"
                    aria-invalid={!!error}
                    aria-describedby={
                      error
                        ? "cancellation-reason-help cancellation-reason-error"
                        : "cancellation-reason-help"
                    }
                    placeholder="说明本次取消的原因"
                    onChange={(event) => {
                      setReason(event.target.value);
                      setError("");
                    }}
                  />
                  <p id="cancellation-reason-help">
                    仅用于管理记录，不向申请人展示。请勿填写手机号或证件号。
                  </p>
                  {error && (
                    <p
                      id="cancellation-reason-error"
                      ref={errorMessage}
                      role="alert"
                    >
                      {error}
                    </p>
                  )}
                </div>
              )}
            </div>
            <div className="review-dialog-actions">
              <Button
                ref={keep}
                type="button"
                className="secondary"
                disabled={pending}
                onClick={close}
              >
                保留申请
              </Button>
              <Button
                type="submit"
                className="review-danger"
                disabled={pending || !cancellable}
              >
                {pending ? "正在取消…" : "取消申请"}
              </Button>
            </div>
          </form>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
