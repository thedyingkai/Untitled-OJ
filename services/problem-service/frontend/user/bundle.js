const element = (tag, text) => {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      const title = element("h1", host.i18n.translate("problem.list.title"));
      const summary = element("p", host.i18n.translate("problem.list.loading"));
      root.replaceChildren(title, summary);
      try {
        const result = await host.client.request("listProblems");
        const problems = Array.isArray(result?.problems) ? result.problems : [];
        summary.textContent = problems.length === 0
          ? host.i18n.translate("problem.list.empty")
          : problems.map((problem) => problem.title).join(", ");
      } catch (error) {
        host.logger.error("load problem list failed", {error: String(error)});
        summary.textContent = host.i18n.translate("problem.list.failed");
      }
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
