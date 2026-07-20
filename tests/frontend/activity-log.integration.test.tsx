import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import LogPanel from "../../src/components/LogPanel";

test("sync log bar keeps the multi-type filter and Manage logs in the header", () => {
  const html = renderToStaticMarkup(
    <LogPanel
      lines={[{
        schema: 1,
        id: "event-one",
        ts: 1,
        level: "success",
        type: "push",
        event: "push.completed",
        message: "Push complete",
      }]}
      typeFilters={["push", "pull"]}
      levelFilter="success"
      onTypeFiltersChange={() => undefined}
      onLevelFilterChange={() => undefined}
      onSearchChange={() => undefined}
      onManage={() => undefined}
      onClose={() => undefined}
    />,
  );

  const headerEnd = html.indexOf("log-panel-body");
  assert.ok(headerEnd > 0);
  const header = html.slice(0, headerEnd);
  assert.match(header, /Filter sync log by types: 2 selected/);
  assert.match(header, />Select all</);
  assert.match(header, />Deselect all</);
  assert.equal(header.match(/type="checkbox"/g)?.length, 7);
  assert.equal(header.match(/checked=""/g)?.length, 2);
  assert.match(header, /Filter sync log by level/);
  assert.match(header, /Search sync log/);
  assert.match(header, />Manage logs</);
  assert.doesNotMatch(header, />clear</);
  assert.match(html, /log-type-push/);
  assert.match(html, /Push complete/);
});

test("sync log exposes retained-history pagination", () => {
  const html = renderToStaticMarkup(
    <LogPanel
      lines={[]}
      hasOlder
      loadingOlder={false}
      onLoadOlder={() => undefined}
      onClose={() => undefined}
    />,
  );

  assert.match(html, />Load older logs</);
});
