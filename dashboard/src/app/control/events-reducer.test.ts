import { describe, expect, it } from "vitest";

import fixtures from "../../../../shared/control-reducer-fixtures.json";
import type { Mission, StoredEvent } from "@/lib/api";
import { eventsToItemsImpl, type ChatItem } from "./events-reducer";

type ExpectedItem = Partial<ChatItem> & { kind: ChatItem["kind"] };

function expectItemsContain(items: ChatItem[], expected: ExpectedItem[]) {
  for (const expectedItem of expected) {
    expect(items).toEqual(
      expect.arrayContaining([expect.objectContaining(expectedItem)]),
    );
  }
}

describe("eventsToItemsImpl shared reducer fixtures", () => {
  for (const fixtureCase of fixtures.cases) {
    it(fixtureCase.name, () => {
      const items = eventsToItemsImpl(
        fixtureCase.events as StoredEvent[],
        fixtures.mission as Mission,
      );
      expectItemsContain(items, fixtureCase.expected as ExpectedItem[]);

      if (fixtureCase.name === "duplicate event ids") {
        expect(items.filter((item) => item.kind === "assistant")).toHaveLength(
          1,
        );
      }
      if (fixtureCase.name === "goal deliverable inference") {
        expect(items.filter((item) => item.kind === "thinking")).toHaveLength(
          0,
        );
      }
    });
  }
});
