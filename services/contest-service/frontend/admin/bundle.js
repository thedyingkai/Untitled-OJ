const h = (tag, text) => {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, element) {
      const heading = h("h1", host.i18n.translate("contest.admin.title"));
      const summary = h("p", host.i18n.translate("contest.admin.loading"));
      element.replaceChildren(heading, summary);
      try {
        const response = await host.client.request("adminListContests");
        summary.textContent = host.i18n.translate("contest.admin.count", {
          count: Array.isArray(response?.items) ? response.items.length : 0,
        });
      } catch (error) {
        host.logger.error("contest admin list failed", {error: String(error)});
        summary.textContent = host.i18n.translate("contest.admin.failed");
      }
      return {dispose() { element.replaceChildren(); }};
    },
    async dispose() {},
  };
}
