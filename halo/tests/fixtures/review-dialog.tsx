import { createRoot } from "react-dom/client";
import { useRef, useState } from "react";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { ReviewDialog } from "../../app/workforce/review-dialog";
import { applicationMaterial } from "../application-material";
import "../../app/styles.css";

const application = applicationMaterial();
application.snapshot.name =
  "陈欧阳司徒慕容夏侯东方闻人长孙南宫诸葛公孙上官司马清和".repeat(2);
function Fixture() {
  const [decision, setDecision] = useState<"approve" | "reject">();
  const [pending, setPending] = useState(false);
  const [submitted, setSubmitted] = useState<string[]>([]);
  const trigger = useRef<HTMLButtonElement>(null);
  return (
    <CSPProvider disableStyleElements>
      <main className="review-layout">
        <h1>审核确认组件</h1>
        <button ref={trigger} onClick={() => setDecision("reject")}>
          驳回申请
        </button>
        <button onClick={() => setDecision("approve")}>批准申请</button>
        <output aria-label="已提交次数">{submitted.length}</output>
        <output aria-label="已提交理由">{submitted.at(-1)}</output>
        {decision && (
          <ReviewDialog
            key={decision}
            application={application}
            decision={decision}
            pending={pending}
            trigger={trigger}
            close={() => setDecision(undefined)}
            submit={(reason) => {
              setSubmitted((values) => [...values, reason]);
              setPending(true);
            }}
          />
        )}
      </main>
    </CSPProvider>
  );
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
