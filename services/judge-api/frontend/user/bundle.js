const node = (tag, text) => {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
};

export async function activate(host) {
  return {
    async mount(_surfaceId, root) {
      const title = node("h1", host.i18n.translate("judge.submissions.title"));
      const summary = node("p", host.i18n.translate("judge.submissions.loading"));
      root.replaceChildren(title, summary);
      try {
        const result = await host.client.request("listSubmissions");
        const submissions = Array.isArray(result?.submissions) ? result.submissions : [];
        summary.textContent = submissions.length === 0
          ? host.i18n.translate("judge.submissions.empty")
          : `${submissions.length} ${host.i18n.translate("judge.submissions.count")}`;
      } catch (error) {
        host.logger.error("load submissions failed", {error: String(error)});
        summary.textContent = host.i18n.translate("judge.submissions.failed");
      }
      return {dispose() { root.replaceChildren(); }};
    },
    async dispose() {},
  };
}
