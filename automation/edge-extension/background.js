'use strict';

// 点击浏览器右上角扩展图标时，在当前标签页 MAIN 世界中打开或收起退货助手面板
chrome.action.onClicked.addListener(async (tab) => {
  if (!tab || !tab.id || !tab.url) return;
  if (!/^https?:\/\//i.test(tab.url)) return;

  try {
    const [{ result: alreadyLoaded } = {}] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      world: 'MAIN',
      func: () => {
        if (window.__RETURN_AUTO__ && typeof window.__RETURN_AUTO__.togglePanel === 'function') {
          window.__RETURN_AUTO__.togglePanel();
          return true;
        }
        return false;
      },
    });

    if (!alreadyLoaded) {
      await chrome.scripting.executeScript({
        target: { tabId: tab.id },
        world: 'MAIN',
        func: () => {
          window.__RETURN_AUTO_EXT_MODE__ = false;
        },
      });
      await chrome.scripting.executeScript({
        target: { tabId: tab.id },
        world: 'MAIN',
        files: ['return_auto.js'],
      });
    }
  } catch (err) {
    console.warn('[退货助手扩展] 唤起面板失败:', err);
  }
});
