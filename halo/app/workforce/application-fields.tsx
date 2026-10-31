import { useId } from "react";
import type { DirectorySelection } from "../protocol/directories";
import type { PreviewInput } from "../protocol/applications";

// Page input, not an HTTP request. Server preview owns all printed text.
export type ApplicationChoices = Readonly<{
  kind: PreviewInput["kind"];
  departmentId: string;
  departmentConfirmed: boolean;
  rosterId: string;
  titleId: string;
  pickup: string;
}>;

function departmentPath(id: string, selection: DirectorySelection): string {
  const names: string[] = [];
  const visited = new Set<string>();
  let current: string | null = id;
  while (current && !visited.has(current)) {
    visited.add(current);
    const department = selection.departments.find(
      (item) => item.id === current,
    );
    if (!department) break;
    names.unshift(department.name);
    current = department.parentId;
  }
  return names.join(" / ");
}

export function ApplicationFields({
  selection,
  subtype,
  pickups,
  value,
  onChange,
  disabled = false,
}: {
  selection: DirectorySelection;
  subtype: number;
  pickups: readonly string[];
  value: ApplicationChoices;
  onChange(value: ApplicationChoices): void;
  disabled?: boolean;
}) {
  const id = useId();
  const rule = selection.rules.find((item) => item.id === subtype);
  const department = selection.departments.find(
    (item) => item.id === value.departmentId,
  );
  const positions = selection.positions.filter(
    (item) => item.departmentId === value.departmentId,
  );
  if (!rule || selection.missing.length > 0)
    return (
      <p role="alert" className="application-notice">
        暂时无法准备工牌申请，请稍后刷新重试。
      </p>
    );
  return (
    <fieldset className="application-fields" disabled={disabled}>
      <legend className="visually-hidden">工牌申请内容</legend>
      <fieldset className="application-kinds">
        <legend>申请类型</legend>
        {(
          [
            ["issuance", "新办"],
            ["duplication", "增办"],
            ["replacement", "换证"],
          ] as const
        ).map(([kind, label]) => (
          <label key={kind}>
            <input
              type="radio"
              name={`${id}-kind`}
              value={kind}
              checked={value.kind === kind}
              onChange={() => onChange({ ...value, kind })}
            />
            <span>{label}</span>
          </label>
        ))}
      </fieldset>
      <div className="application-field">
        <label htmlFor={`${id}-department`}>科室</label>
        <select
          id={`${id}-department`}
          value={value.departmentId}
          required
          onChange={(event) =>
            onChange({
              ...value,
              departmentId: event.currentTarget.value,
              departmentConfirmed: false,
              rosterId: "",
            })
          }
        >
          <option value="">请选择科室</option>
          {selection.groups.map((group) => (
            <optgroup key={group.id} label={group.name}>
              {selection.departments
                .filter((item) => item.groupId === group.id)
                .map((item) => (
                  <option key={item.id} value={item.id}>
                    {departmentPath(item.id, selection)}
                  </option>
                ))}
            </optgroup>
          ))}
        </select>
        {department && (
          <p className="application-selected">
            已选科室：{departmentPath(department.id, selection)}
          </p>
        )}
        <label className="application-checkbox">
          <input
            type="checkbox"
            required
            checked={value.departmentConfirmed}
            disabled={!department || disabled}
            onChange={(event) =>
              onChange({
                ...value,
                departmentConfirmed: event.currentTarget.checked,
              })
            }
          />
          <span>本次工牌使用以上科室。</span>
        </label>
        <p className="field-hint">请按本次工牌需要选择科室。</p>
      </div>
      {rule.position && (
        <div className="application-field">
          <label htmlFor={`${id}-position`}>职务（选填）</label>
          <select
            id={`${id}-position`}
            value={value.rosterId}
            disabled={!department || disabled}
            onChange={(event) =>
              onChange({ ...value, rosterId: event.currentTarget.value })
            }
          >
            <option value="">不选职务</option>
            {positions.map((item) => (
              <option key={item.entryId} value={item.entryId}>
                {item.name}
              </option>
            ))}
          </select>
          {value.rosterId && (
            <p className="application-selected">
              已选职务：
              {positions.find((item) => item.entryId === value.rosterId)?.name}
            </p>
          )}
        </div>
      )}
      <div className="application-field">
        <label htmlFor={`${id}-title`}>职称（选填）</label>
        <select
          id={`${id}-title`}
          value={value.titleId}
          onChange={(event) =>
            onChange({ ...value, titleId: event.currentTarget.value })
          }
        >
          <option value="">不选职称，工牌显示“无”</option>
          {selection.disciplines.map((discipline) => (
            <optgroup
              key={discipline.id}
              label={`${selection.categories.find((item) => item.id === discipline.categoryId)?.name ?? ""} / ${discipline.name}`}
            >
              {selection.titles
                .filter((item) => item.disciplineId === discipline.id)
                .map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.name}
                  </option>
                ))}
            </optgroup>
          ))}
        </select>
        {value.titleId && (
          <p className="application-selected">
            已选职称：
            {selection.titles.find((item) => item.id === value.titleId)?.name}
          </p>
        )}
      </div>
      <div className="application-field">
        <label htmlFor={`${id}-pickup`}>领取地点</label>
        <select
          id={`${id}-pickup`}
          value={value.pickup}
          required
          onChange={(event) =>
            onChange({ ...value, pickup: event.currentTarget.value })
          }
        >
          <option value="">请选择领取地点</option>
          {pickups.map((pickup) => (
            <option key={pickup} value={pickup}>
              {pickup}
            </option>
          ))}
        </select>
        {value.pickup && (
          <p className="application-selected">领取地点：{value.pickup}</p>
        )}
        <p className="field-hint">领取地点不打印在工牌上。</p>
      </div>
    </fieldset>
  );
}
