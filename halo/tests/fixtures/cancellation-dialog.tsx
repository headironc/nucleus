import { createRoot } from "react-dom/client";
import { useRef, useState } from "react";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { CancellationDialog } from "../../app/workforce/cancellation-dialog";
import type { ApplicationDetail } from "../../app/protocol/applications";
import { applicationMaterial, accountId } from "../application-material";
import "../../app/styles.css";

const parameters = new URLSearchParams(location.search);
const mode = parameters.get("mode") === "manager" ? "manager" : "self";
const full = applicationMaterial();
full.snapshot.name =
  "陈欧阳司徒慕容夏侯东方闻人长孙南宫诸葛公孙上官司马清和".repeat(2);
const approved = {
  ...full,
  status: "approved" as const,
  reviewerId: accountId,
  reviewerName: "林*",
  reviewedAt: "2026-10-31T01:15:00Z",
};
const item = parameters.get("status") === "approved" ? approved : full;
const { id, kind, status, submittedAt } = item;
const application: ApplicationDetail["application"] = parameters.has(
  "restricted",
)
  ? { id, kind, status, submittedAt }
  : item;
function Fixture() {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [submissions, setSubmissions] = useState<string[]>([]);
  const trigger = useRef<HTMLButtonElement>(null);
  return (
    <CSPProvider disableStyleElements>
      <main className="review-layout">
        <h1>工牌取消确认</h1>
        <button ref={trigger} onClick={() => setOpen(true)}>
          取消申请
        </button>
        <output aria-label="已提交次数">{submissions.length}</output>
        <output aria-label="已提交说明">{submissions.at(-1)}</output>
        {open && (
          <CancellationDialog
            application={application}
            mode={mode}
            pending={pending}
            trigger={trigger}
            close={() => setOpen(false)}
            submit={(reason) => {
              setSubmissions((values) => [...values, reason]);
              setPending(true);
            }}
          />
        )}
      </main>
    </CSPProvider>
  );
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
