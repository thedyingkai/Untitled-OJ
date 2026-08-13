const h = (tag, text) => {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
};

export async function activate(host) {
  const subscriptions = [];
  return {
    async mount(_surfaceId, element) {
      const heading = h("h1", host.i18n.translate("contest.list.title"));
      const status = h("p", host.i18n.translate("contest.list.loading"));
      element.replaceChildren(heading, status);
      try {
        const response = await host.client.request("listContests");
        const contests = Array.isArray(response?.items) ? response.items : [];
        status.textContent = contests.length === 0
          ? host.i18n.translate("contest.list.empty")
          : contests.map((contest) => contest.title).join(", ");
      } catch (error) {
        host.logger.error("contest list failed", {error: String(error)});
        status.textContent = host.i18n.translate("contest.list.failed");
      }
      return {dispose() { element.replaceChildren(); }};
    },
    async dispose() {
      for (const subscription of subscriptions.splice(0)) subscription.dispose();
    },
  };
}
