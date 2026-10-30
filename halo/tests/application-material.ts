import type {
  Application,
  ApplicationSnapshot,
  ApplicationTemplate,
  PhotoMetadata,
} from "../app/protocol/applications";
import type { DirectorySelection } from "../app/protocol/directories";
import type { FixedNumber } from "../app/protocol/number";

export const applicationId = (n: number) =>
  `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
export const accountId = applicationId(1),
  otherAccountId = applicationId(2);
export const fixedNumber = {
  id: applicationId(3),
  subtype: 100,
  year: 2026,
  serial: 5,
} satisfies FixedNumber;
const rectangle = (
  xUm: number,
  yUm: number,
  widthUm: number,
  heightUm: number,
) => ({ xUm, yUm, widthUm, heightUm });
const textArea = (y: number, height: number, font: number) => ({
  area: rectangle(35000, y, 45000, height),
  fontSizeUm: font,
  lineHeightUm: font * 1.25,
  wrap: true,
});
export const templateMaterial: ApplicationTemplate = {
  code: "ok",
  template: {
    backgroundId: applicationId(20),
    fontId: applicationId(21),
    layout: {
      widthUm: 85000,
      heightUm: 54000,
      photo: rectangle(5000, 8000, 25000, 35000),
      name: textArea(7000, 9000, 3500),
      number: textArea(17500, 6000, 3500),
      department: textArea(25500, 12000, 2500),
      title: textArea(40000, 8000, 2500),
    },
  },
  pickups: ["主院区综合服务大厅一层工牌办理窗口", "分院区行政服务中心"],
};
export const directoryMaterial: DirectorySelection = {
  code: "ok",
  groups: [{ id: applicationId(4), name: "临床服务", index: 0 }],
  departments: [
    {
      id: applicationId(5),
      groupId: applicationId(4),
      parentId: null,
      name: "综合临床诊疗与长期健康照护协作中心",
      index: 0,
    },
    {
      id: applicationId(6),
      groupId: applicationId(4),
      parentId: null,
      name: "康复医学中心",
      index: 1,
    },
  ],
  positions: [
    {
      departmentId: applicationId(5),
      entryId: applicationId(7),
      positionId: applicationId(8),
      name: "主任",
      index: 0,
    },
  ],
  categories: [{ id: applicationId(9), name: "卫生技术", index: 0 }],
  disciplines: [
    {
      id: applicationId(10),
      categoryId: applicationId(9),
      name: "临床医学",
      index: 0,
    },
  ],
  titles: [
    {
      id: applicationId(11),
      disciplineId: applicationId(10),
      name: "主任医师",
      index: 0,
    },
  ],
  rules: (
    [
      100, 200, 300, 400, 500, 600, 700, 801, 802, 803, 804, 901, 902, 903, 904,
      1000,
    ] as const
  ).map((id) => ({
    id,
    departmentId: applicationId(5),
    position: id !== 902,
    selectable: true,
  })),
  missing: [],
};
export const photoMaterial: PhotoMetadata = {
  photoId: applicationId(30),
  resourceId: applicationId(31),
  width: 800,
  height: 1120,
  mediaType: "image/webp",
};
export function snapshotMaterial(): ApplicationSnapshot {
  const source = {
    name: "测试同事",
    number: "12605",
    department: directoryMaterial.departments[0].name,
    title: "无",
  };
  const line = (text: string, y: number) => [
    {
      start: 0,
      end: new TextEncoder().encode(text).byteLength,
      xUm: 35000,
      baselineUm: y,
      widthUm: 20000,
      direction: "ltr" as const,
    },
  ];
  return {
    name: source.name,
    number: {
      subtype: fixedNumber.subtype,
      year: fixedNumber.year,
      serial: fixedNumber.serial,
    },
    printedNumber: source.number,
    department: {
      id: applicationId(5),
      name: source.department,
      printed: source.department,
    },
    position: null,
    title: null,
    printedTitle: source.title,
    photoId: photoMaterial.photoId,
    template: templateMaterial.template,
    pickup: templateMaterial.pickups[0],
    text: {
      name: line(source.name, 12500),
      number: line(source.number, 21500),
      department: line(source.department, 30000),
      title: line(source.title, 45000),
    },
  };
}
export function applicationMaterial(): Application {
  return {
    id: applicationId(40),
    kind: "issuance",
    status: "submitted",
    submittedAt: "2026-10-30T18:00:00Z",
    snapshot: snapshotMaterial(),
    photo: photoMaterial,
  };
}
export function accountMaterial(id = accountId) {
  return {
    code: "ok",
    account: {
      id,
      personId: id,
      revisionId: applicationId(50),
      name: id === accountId ? "测试同事" : "另一位同事",
      idNumber: { suffix: "002X" },
      binding: null,
      avatar: {
        revision: accountId,
        href: `https://fixture.invalid/api/v1/avatars/${"a".repeat(64)}.webp?key_id=7&expires_at=2051222400&signature=${"_".repeat(42)}8`,
        expiresAt: "2035-01-01T00:00:00Z",
        mediaType: "image/webp",
        width: 512,
        height: 512,
      },
    },
  };
}
