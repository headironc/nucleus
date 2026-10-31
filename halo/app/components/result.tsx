import type { ReactNode } from "react";
import { ShieldCheck } from "lucide-react";

export function Result({
  title,
  copy,
  note,
  icon,
  children,
  footer,
}: {
  title: string;
  copy: string;
  note: string;
  icon: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <div className="result">
      {icon}
      <h1 tabIndex={-1} data-result-title>
        {title}
      </h1>
      <p className="result-copy">{copy}</p>
      {note && (
        <div className="notice">
          <ShieldCheck size={20} aria-hidden="true" />
          <p>{note}</p>
        </div>
      )}
      <div className="result-actions">{children}</div>
      {footer && <p className="scope-note">{footer}</p>}
    </div>
  );
}
