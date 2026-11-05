import { Service } from "@superrouter/contracts";
import { demoService } from "../../scripts/demo-config.js";
export const services = {
  chat: {
    id: "paid-chat",
    unit: "input/output tokens",
    rate: "輸入 0.000002／輸出 0.000003 credits/token",
    max: 1,
  },
  image: {
    id: "paid-image",
    kind: "image" as const,
    unit: "張",
    rate: "0.2 credits／張",
    max: 4,
  },
  video: {
    id: "paid-video",
    kind: "video" as const,
    unit: "秒",
    rate: "0.05 credits／秒",
    max: 30,
  },
  audio: {
    id: "paid-audio",
    kind: "audio" as const,
    unit: "秒",
    rate: "0.01 credits／秒",
    max: 60,
  },
  task: {
    id: "paid-task",
    kind: "task" as const,
    unit: "次成功",
    rate: "0.1 credits／次成功",
    max: 1,
  },
};
export const catalog = [
  {
    id: "starter-v1",
    name: "Starter",
    description: "適合日常對話與小量創作。",
    plan: {
      id: "starter",
      version: 1,
      period_days: 30,
      asset: "credit:paid-demo",
      credits: "10000000000",
      service_ids: Object.values(services).map((s) => s.id),
      amount_minor: "9900",
      currency: "TWD",
    },
  },
  {
    id: "creator-v1",
    name: "Creator",
    description: "用同一份額度，探索更多媒體任務。",
    plan: {
      id: "creator",
      version: 1,
      period_days: 30,
      asset: "credit:paid-demo",
      credits: "30000000000",
      service_ids: Object.values(services).map((s) => s.id),
      amount_minor: "24900",
      currency: "TWD",
    },
  },
];
export const chatService = Service.parse({
  ...demoService,
  id: services.chat.id,
  requires_subscription: true,
  price: {
    ...demoService.price,
    asset: "credit:paid-demo",
    input: "2000",
    cached: "1000",
    cache_write: "2000",
    output: "3000",
  },
});
export const taskServices = Object.values(services)
  .filter((s) => "kind" in s)
  .map((s) => {
    const kind = (s as { kind: "image" | "video" | "audio" | "task" }).kind;
    const meter =
      kind === "image"
        ? "images"
        : kind === "video"
          ? "video_ms"
          : kind === "audio"
            ? "audio_ms"
            : "tasks";
    const price = {
      asset: "credit:paid-demo",
      input: "0",
      cached: "0",
      cache_write: "0",
      output: "0",
      per: 1,
      media: {
        [meter]: {
          atoms:
            kind === "image"
              ? "200000000"
              : kind === "video"
                ? "50000000"
                : kind === "audio"
                  ? "10000000"
                  : "100000000",
          per: kind === "video" || kind === "audio" ? 1000 : 1,
        },
      },
    };
    const max_usage = {
      [meter]: s.max * (kind === "video" || kind === "audio" ? 1000 : 1),
    };
    return Service.parse({
      ...demoService,
      id: s.id,
      requires_subscription: true,
      task_kind: kind,
      price,
      max_provider_cost: "1000000000",
      offerings: [
        {
          ...demoService.offerings[0],
          id: "sandbox-" + kind,
          provider: "mock-task",
          task_kind: kind,
          max_usage,
          max_input_tokens: 1,
          max_output_tokens: 1,
          price: {
            ...price,
            asset: "USD",
            media: { [meter]: { atoms: "1", per: 1 } },
          },
        },
      ],
    });
  });
