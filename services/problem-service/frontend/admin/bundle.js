const element = (tag, text) => {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      const title = element("h1", host.i18n.translate("problem.admin.title"));
      const summary = element("p", host.i18n.translate("problem.admin.loading"));
      root.replaceChildren(title, summary);
      try {
        const result = await host.client.request("adminListProblems");
        const count = Array.isArray(result?.problems) ? result.problems.length : 0;
        summary.textContent = host.i18n.translate("problem.admin.count", {count});
      } catch (error) {
        host.logger.error("load problem admin list failed", {error: String(error)});
        summary.textContent = host.i18n.translate("problem.admin.failed");
      }
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
