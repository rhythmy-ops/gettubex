import { describe, expect, it } from "vitest";
import { detectPlatform, extractFirstUrl, looksLikeChannel, looksLikeList } from "./platforms";

describe("platform detection", () => {
  it.each([
    ["https://youtu.be/demo", "youtube"],
    ["https://www.youtube.com/watch?v=demo", "youtube"],
    ["https://www.bilibili.com/video/BV1xx", "bilibili"],
    ["https://b23.tv/demo", "bilibili"],
    ["复制打开抖音 https://v.douyin.com/demo/ 看视频", "douyin"],
    ["https://x.com/openai/status/123", "twitter"],
    ["https://t.co/demo", "twitter"],
    ["复制红果 https://applink.novelquickapp.com/example", "hongguo"],
    ["https://hongguoduanju.com/player/123/456", "hongguo"],
    ["https://hongguoduanju.com.example.test/player/123", "unknown"],
  ])("detects %s", (input, expected) => {
    expect(detectPlatform(input)).toBe(expected);
  });

  it("does not accept lookalike domains", () => {
    expect(detectPlatform("https://youtube.com.example.test/video")).toBe("unknown");
  });

  it("extracts a URL from share text", () => {
    expect(extractFirstUrl("看看这个 https://v.douyin.com/abc/，很好看"))
      .toBe("https://v.douyin.com/abc/");
  });
});

describe("list vs channel detection", () => {
  it.each([
    ["https://www.youtube.com/playlist?list=PL123", true],
    ["https://www.youtube.com/watch?v=abc&list=PL123", false], // 带 v 参数按单视频
    ["https://www.youtube.com/watch?list=PL123", true],
    ["https://www.bilibili.com/video/BV1G48M6XEBt", true], // 无 p 视为多 P
    ["https://www.bilibili.com/video/BV1G48M6XEBt?p=3", false],
    ["https://space.bilibili.com/123/favlist?fid=456", true],
    ["https://space.bilibili.com/123/lists/456?type=season", true],
    ["https://www.youtube.com/watch?v=abc", false],
    ["https://v.douyin.com/abc/", false],
    ["https://hongguoduanju.com/player/123/456", true],
    ["https://applink.novelquickapp.com/example", true],
  ])("list detection for %s", (input, expected) => {
    expect(looksLikeList(input)).toBe(expected);
  });

  it.each([
    ["https://www.youtube.com/@NASA/videos", true],
    ["https://www.youtube.com/channel/UC123/videos", true],
    ["https://www.youtube.com/@NASA", false],
    ["https://space.bilibili.com/3546883796503085/video", true],
    ["https://space.bilibili.com/123/favlist?fid=456", false],
    ["https://space.bilibili.com/123", false],
    ["https://hongguoduanju.com/player/123/456", false],
  ])("channel detection for %s", (input, expected) => {
    expect(looksLikeChannel(input)).toBe(expected);
  });
});
