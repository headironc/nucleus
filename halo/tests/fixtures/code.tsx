import { useState } from "react";
import { createRoot } from "react-dom/client";
import { CSPProvider } from "@base-ui/react/csp-provider";
import { CodeInput } from "../../app/authentication/code";
import "../../app/styles.css";

function Fixture() {
  const [value, setValue] = useState("");
  const [changes, setChanges] = useState(0);
  const [disabled, setDisabled] = useState(false);
  const [invalid, setInvalid] = useState(false);
  const [incomplete, setIncomplete] = useState(false);
  return (
    <CSPProvider disableStyleElements>
      <main className="authentication-layout compact">
        <section className="authentication-card">
          <h1>验证码组件夹具</h1>
          <CodeInput
            value={value}
            onChange={(next) => {
              setValue(next);
              setChanges((count) => count + 1);
            }}
            disabled={disabled}
            invalid={invalid}
            incomplete={incomplete}
          />
        </section>
        <section aria-label="夹具控制">
          <output aria-label="受控值">{value}</output>
          <output aria-label="输入变更次数">{changes}</output>
          <div>
            <button onClick={() => setValue("012345")}>设置示例值</button>
            <button onClick={() => setValue("")}>清空</button>
          </div>
          <label>
            <input
              type="checkbox"
              checked={disabled}
              onChange={(event) => setDisabled(event.target.checked)}
            />
            禁用
          </label>
          <label>
            <input
              type="checkbox"
              checked={invalid}
              onChange={(event) => setInvalid(event.target.checked)}
            />
            错码
          </label>
          <label>
            <input
              type="checkbox"
              checked={incomplete}
              onChange={(event) => setIncomplete(event.target.checked)}
            />
            缺位
          </label>
        </section>
      </main>
    </CSPProvider>
  );
}

createRoot(document.getElementById("fixture")!).render(<Fixture />);
