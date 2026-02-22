// WebView注入脚本 - 用于修复微信公众号文章显示问题

// 方案1: 在页面加载完成后注入
function fixWeChatArticleDisplay() {
  // 移除隐藏样式
  const jsContent = document.getElementById('js_content');
  if (jsContent) {
    jsContent.style.visibility = 'visible';
    jsContent.style.opacity = '1';
  }

  // 修复所有图片
  document.querySelectorAll('img').forEach(img => {
    img.style.visibility = 'visible';
    img.style.opacity = '1';
    img.style.width = 'auto';
    img.style.height = 'auto';
  });

  // 修复所有section
  document.querySelectorAll('section').forEach(sec => {
    sec.style.visibility = 'visible';
    sec.style.opacity = '1';
  });
}

// 页面加载完成后执行
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', fixWeChatArticleDisplay);
} else {
  fixWeChatArticleDisplay();
}

// 方案2: 添加CSS到页面
const style = document.createElement('style');
style.textContent = `
  #js_content {
    visibility: visible !important;
    opacity: 1 !important;
  }
  img, .rich_pages.wxw-img {
    visibility: visible !important;
    opacity: 1 !important;
    width: auto !important;
    min-width: 100px !important;
    height: auto !important;
  }
  section {
    visibility: visible !important;
    opacity: 1 !important;
  }
`;
document.head.appendChild(style);