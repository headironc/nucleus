import { useState } from "react";
import { createRoot } from "react-dom/client";
import { CSPProvider } from "@base-ui/react/csp-provider";
import type { DirectorySelection } from "../../app/protocol/directories";
import { ApplicationFields, type ApplicationChoices } from "../../app/workforce/application-fields";
import { ApplicationPhoto } from "../../app/workforce/photo";
import { ApplicationPhotoPanel } from "../../app/workforce/photo-panel";
import "../../app/styles.css";

declare global { interface Window { applicationChoices: ApplicationChoices; } }
const id = (number: number) => `00000000-0000-4000-8000-${String(number).padStart(12, "0")}`;
const selection: DirectorySelection = {
  code: "ok", groups: [{ id: id(1), name: "诊疗服务", index: 0 }],
  departments: [
    { id: id(2), groupId: id(1), parentId: null, name: "综合临床医学诊疗与长期健康照护协作中心", index: 0 },
    { id: id(3), groupId: id(1), parentId: id(2), name: "复杂慢性疾病多学科协作及康复随访门诊", index: 1 },
  ],
  positions: [{ departmentId: id(2), entryId: id(4), positionId: id(5), name: "临床诊疗及患者全程照护协调负责人", index: 0 }],
  categories: [{ id: id(6), name: "卫生技术", index: 0 }],
  disciplines: [{ id: id(7), categoryId: id(6), name: "临床医学", index: 0 }],
  titles: [{ id: id(8), disciplineId: id(7), name: "全科医学主任医师", index: 0 }],
  rules: [{ id: 100, departmentId: id(2), position: true, selectable: true }, { id: 902, departmentId: id(2), position: false, selectable: true }], missing: [],
};
const photo = new ApplicationPhoto();
function Fixture() {
  const [value, setValue] = useState<ApplicationChoices>({ kind: "issuance", departmentId: id(2), departmentConfirmed: false, rosterId: "", titleId: "", pickup: "" });
  window.applicationChoices = value;
  return <main className="application-layout">
    <h1>申请工牌</h1>
    <p className="application-introduction">选择工牌内容和照片，预览确认后提交。</p>
    <ApplicationFields selection={location.pathname === "/missing" ? { ...selection, rules: selection.rules.filter((rule) => rule.id !== 902), missing: [902] } : selection} subtype={location.pathname === "/intern" ? 902 : 100} pickups={["主院区综合服务大厅一层工牌办理窗口", "分院区行政服务中心"]} value={value} onChange={setValue} />
    <ApplicationPhotoPanel photo={photo} frame={{ width: 3, height: 4 }} />
  </main>;
}
createRoot(document.getElementById("fixture")!).render(<CSPProvider disableStyleElements><Fixture /></CSPProvider>);
