import type { CharacterId, EmotionTag } from "../types/ipc";

export type { CharacterId };
export type AvatarEngineMode = "vector" | "live2d";

export interface CubismParams {
  ParamAngleX: number;
  ParamAngleY: number;
  ParamAngleZ: number;
  ParamEyeLOpen: number;
  ParamEyeROpen: number;
  ParamEyeLSmile: number;
  ParamEyeRSmile: number;
  ParamEyeBallX: number;
  ParamEyeBallY: number;
  ParamBrowLY: number;
  ParamBrowRY: number;
  ParamBrowLAngle: number;
  ParamBrowRAngle: number;
  ParamMouthOpenY: number;
  ParamMouthForm: number;
  ParamCheek: number;
  ParamBreath: number;
  ParamBodyAngleX: number;
  ParamBodyAngleY: number;
  ParamBodyAngleZ: number;
  ParamHairFront: number;
  ParamHairSide: number;
  ParamHairBack: number;
  ParamAccessorySway: number;
  ParamTearSheen: number;
  speechEnergy: number;
  ahogeLift: number;
}

export interface EmotionVisualMeta {
  tag: EmotionTag;
  labelEn: string;
  labelTh: string;
  accentColor: string;
  pillBg: string;
  auraColor: string;
}

export const ALL_EMOTIONS: EmotionTag[] = [
  "joy",
  "sadness",
  "shy",
  "surprised",
  "thinking",
  "empathetic",
  "neutral",
];

export const EMOTION_META: Record<EmotionTag, EmotionVisualMeta> = {
  joy: {
    tag: "joy",
    labelEn: "Joy",
    labelTh: "ดีใจ",
    accentColor: "#E77C5B",
    pillBg: "rgba(255, 198, 152, 0.28)",
    auraColor: "rgba(255, 200, 148, 0.34)",
  },
  sadness: {
    tag: "sadness",
    labelEn: "Sadness",
    labelTh: "เศร้า",
    accentColor: "#7492C7",
    pillBg: "rgba(130, 162, 224, 0.24)",
    auraColor: "rgba(128, 160, 225, 0.28)",
  },
  shy: {
    tag: "shy",
    labelEn: "Shy",
    labelTh: "เขิน",
    accentColor: "#E86C8D",
    pillBg: "rgba(255, 150, 182, 0.28)",
    auraColor: "rgba(255, 148, 184, 0.36)",
  },
  surprised: {
    tag: "surprised",
    labelEn: "Surprised",
    labelTh: "ตกใจ",
    accentColor: "#E3A43B",
    pillBg: "rgba(255, 212, 118, 0.28)",
    auraColor: "rgba(255, 214, 110, 0.32)",
  },
  thinking: {
    tag: "thinking",
    labelEn: "Thinking",
    labelTh: "กำลังคิด",
    accentColor: "#7E78C9",
    pillBg: "rgba(154, 146, 232, 0.25)",
    auraColor: "rgba(156, 148, 235, 0.28)",
  },
  empathetic: {
    tag: "empathetic",
    labelEn: "Empathetic",
    labelTh: "ห่วงใย",
    accentColor: "#D97284",
    pillBg: "rgba(247, 164, 178, 0.28)",
    auraColor: "rgba(245, 172, 182, 0.35)",
  },
  neutral: {
    tag: "neutral",
    labelEn: "Neutral",
    labelTh: "สบายๆ",
    accentColor: "#5E9B96",
    pillBg: "rgba(130, 198, 190, 0.24)",
    auraColor: "rgba(152, 212, 206, 0.24)",
  },
};

export function createDefaultCubismParams(): CubismParams {
  return {
    ParamAngleX: 0,
    ParamAngleY: 0,
    ParamAngleZ: 0,
    ParamEyeLOpen: 1,
    ParamEyeROpen: 1,
    ParamEyeLSmile: 0,
    ParamEyeRSmile: 0,
    ParamEyeBallX: 0,
    ParamEyeBallY: 0,
    ParamBrowLY: 0,
    ParamBrowRY: 0,
    ParamBrowLAngle: 0,
    ParamBrowRAngle: 0,
    ParamMouthOpenY: 0,
    ParamMouthForm: 0.14,
    ParamCheek: 0.12,
    ParamBreath: 0.5,
    ParamBodyAngleX: 0,
    ParamBodyAngleY: 0,
    ParamBodyAngleZ: 0,
    ParamHairFront: 0,
    ParamHairSide: 0,
    ParamHairBack: 0,
    ParamAccessorySway: 0,
    ParamTearSheen: 0,
    speechEnergy: 0,
    ahogeLift: 0,
  };
}
