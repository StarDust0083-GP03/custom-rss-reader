/**
 * Webview 测试脚本
 * 此脚本需要在浏览器控制台中运行
 */

console.log('='.repeat(60));
console.log('开始 Webview 测试');
console.log('='.repeat(60));

// 测试 1: 基本功能测试
console.log('\n[测试 1] 测试基本 iframe 功能...');
try {
  if (typeof testIframeBasic === 'function') {
    console.log('✓ testIframeBasic 函数存在');
    console.log('  提示: 在控制台中运行 testIframeBasic() 来测试基本 iframe 功能');
  } else {
    console.error('✗ testIframeBasic 函数不存在');
  }
} catch (e) {
  console.error('✗ 测试失败:', e);
}

// 测试 2: Webview 功能测试
console.log('\n[测试 2] 测试 webview 功能...');
try {
  if (typeof testWebview === 'function') {
    console.log('✓ testWebview 函数存在');
    console.log('  提示: 在控制台中运行 testWebview() 来测试完整的 webview 流程');
    console.log('  或者运行 testWebview("你的URL") 来测试特定网址');
  } else {
    console.error('✗ testWebview 函数不存在');
  }
} catch (e) {
  console.error('✗ 测试失败:', e);
}

// 测试 3: 检查 DOM 元素
console.log('\n[测试 3] 检查必要的 DOM 元素...');
const detailContent = document.getElementById('detail-content');
if (detailContent) {
  console.log('✓ detail-content 元素存在');
} else {
  console.error('✗ detail-content 元素不存在');
}

// 测试 4: 检查 Tauri invoke 是否可用
console.log('\n[测试 4] 检查 Tauri API...');
try {
  if (typeof window.__TAURI__ !== 'undefined' || typeof invoke !== 'undefined') {
    console.log('✓ Tauri API 可用');
  } else {
    console.warn('⚠ Tauri API 不可用 (可能在浏览器中运行)');
  }
} catch (e) {
  console.warn('⚠ 无法检查 Tauri API:', e);
}

// 测试 5: 检查 CSS 样式
console.log('\n[测试 5] 检查 CSS 样式...');
const testElement = document.createElement('div');
testElement.className = 'webpage-translation';
document.body.appendChild(testElement);
const styles = window.getComputedStyle(testElement);

console.log('  webpage-translation 样式:');
console.log('    - max-height:', styles.maxHeight);
console.log('    - overflow-y:', styles.overflowY);
console.log('    - box-sizing:', styles.boxSizing);

document.body.removeChild(testElement);

console.log('\n' + '='.repeat(60));
console.log('测试完成！');
console.log('='.repeat(60));
console.log('\n下一步操作:');
console.log('1. 如果在 Tauri 应用中，运行 testWebview() 来测试 webview');
console.log('2. 打开开发者工具查看详细的加载日志');
console.log('3. 检查控制台是否有错误信息');
