import { describe, expect, it } from "vitest";

import {
  appendUnpersistedLiveTail,
  deriveItemViews,
  type ChatItem,
} from "./control-client";

const streamItem: Extract<ChatItem, { kind: "stream" }> = {
  id: "text_delta_latest",
  kind: "stream",
  content: "Visible assistant draft",
  done: false,
};

const thinkingItem: Extract<ChatItem, { kind: "thinking" }> = {
  id: "thinking-1",
  kind: "thinking",
  content: "Typed provider reasoning",
  done: false,
};

describe("deriveItemViews", () => {
  it("routes text_delta stream rows to the side panel when open", () => {
    const views = deriveItemViews([streamItem], true);

    expect(views.thinkingItems).toEqual([streamItem]);
    expect(views.thinkingItemsCount).toBe(0);
    expect(views.hasActiveThinking).toBe(false);
    expect(views.groupedItems).toEqual([]);
  });

  it("routes real thinking rows to the side panel when open", () => {
    const views = deriveItemViews([streamItem, thinkingItem], true);

    expect(views.thinkingItems).toEqual([streamItem, thinkingItem]);
    expect(views.thinkingItemsCount).toBe(1);
    expect(views.hasActiveThinking).toBe(true);
    expect(views.groupedItems).toEqual([]);
  });

  it("does not drop a real thought when a stream row has matching text", () => {
    const matchingStream: Extract<ChatItem, { kind: "stream" }> = {
      ...streamItem,
      content: thinkingItem.content,
    };

    const views = deriveItemViews([thinkingItem, matchingStream], true);

    expect(views.thinkingItems).toEqual([thinkingItem, matchingStream]);
    expect(views.thinkingItemsCount).toBe(1);
    expect(views.groupedItems).toEqual([]);
  });

  it("keeps thinking and stream rows inline when the side panel is closed", () => {
    const views = deriveItemViews([streamItem, thinkingItem], false);

    expect(views.thinkingItems).toEqual([thinkingItem]);
    expect(views.groupedItems).toEqual([
      {
        kind: "thinking_group",
        groupId: streamItem.id,
        thoughts: [streamItem, thinkingItem],
      },
    ]);
  });
});

describe("appendUnpersistedLiveTail", () => {
  const userItem: Extract<ChatItem, { kind: "user" }> = {
    id: "user-1",
    kind: "user",
    content: "Start",
    timestamp: 1,
  };
  const assistantItem: Extract<ChatItem, { kind: "assistant" }> = {
    id: "assistant-1",
    kind: "assistant",
    content: "Final answer",
    success: true,
    costCents: 0,
    costSource: "unknown",
    model: null,
    timestamp: 2,
  };

  it("does not append a stale live stream after a persisted assistant reply", () => {
    const views = appendUnpersistedLiveTail(
      [userItem, assistantItem],
      [userItem, streamItem],
    );

    expect(views).toEqual([userItem, assistantItem]);
  });

  it("does not append a live stream whose content already persisted as assistant", () => {
    const matchingStream: Extract<ChatItem, { kind: "stream" }> = {
      ...streamItem,
      content: assistantItem.content,
    };

    const views = appendUnpersistedLiveTail(
      [userItem, assistantItem],
      [userItem, matchingStream],
    );

    expect(views).toEqual([userItem, assistantItem]);
  });

  it("keeps a genuine live stream when no persisted assistant has arrived", () => {
    const views = appendUnpersistedLiveTail([userItem], [userItem, streamItem]);

    expect(views).toEqual([userItem, streamItem]);
  });
});
