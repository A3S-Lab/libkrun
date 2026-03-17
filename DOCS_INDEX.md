# Windows WHPX 内核启动项目文档索引

本项目致力于在Windows WHPX虚拟化平台上启动Linux内核,最终目标是运行nginx。

---

## 📚 文档导航

### 🎯 快速开始 (Start Here)

1. **[CURRENT_STATUS.md](CURRENT_STATUS.md)** - 当前状态和下一步行动
   - 最新进展总结
   - 立即行动步骤
   - 如何运行测试
   - 预期结果和分析方法

2. **[QUICK_REFERENCE.md](QUICK_REFERENCE.md)** - 快速参考
   - 常用命令
   - 关键文件位置
   - 快速故障排除

### 📖 核心文档

3. **[WINDOWS_WHPX_README.md](WINDOWS_WHPX_README.md)** - 项目README
   - 项目概述和目标
   - 架构说明
   - 技术栈
   - 已完成的里程碑

4. **[RIP_TRACKING_GUIDE.md](RIP_TRACKING_GUIDE.md)** - RIP跟踪系统指南
   - 跟踪系统功能说明
   - 如何运行测试
   - 输出分析方法
   - 调试技巧

5. **[APIC_IMPROVEMENT.md](APIC_IMPROVEMENT.md)** - APIC改进说明 🆕
   - LAPIC寄存器模拟实现
   - 问题分析和解决方案
   - 寄存器值详细说明
   - 测试和验证方法

6. **[TROUBLESHOOTING.md](TROUBLESHOOTING.md)** - 故障排除指南
   - 常见问题和解决方案
   - 构建问题
   - 运行时问题
   - 环境配置

7. **[IMPROVEMENT_SUMMARY.md](IMPROVEMENT_SUMMARY.md)** - 改进总结
   - 本次会话完成的工作
   - 技术细节和实现
   - 使用示例
   - 关键洞察

### 🔧 工具和脚本

- **run_test_proper.ps1** - PowerShell测试运行器 (推荐)
- **run_test.bat** - CMD批处理测试运行器
- **download_kernel.ps1** - 内核下载脚本

---

## 📋 文档阅读顺序

### 新用户 (First Time)

1. **WINDOWS_WHPX_README.md** - 了解项目背景
2. **CURRENT_STATUS.md** - 了解当前状态
3. **RIP_TRACKING_GUIDE.md** - 学习如何运行测试
4. **TROUBLESHOOTING.md** - 遇到问题时查阅

### 开发者 (Developer)

1. **CURRENT_STATUS.md** - 快速了解当前进展
2. **IMPROVEMENT_SUMMARY.md** - 了解最新改进
3. **RIP_TRACKING_GUIDE.md** - 使用跟踪工具
4. **README.md** - 深入技术细节

### 调试问题 (Debugging)

1. **QUICK_REFERENCE.md** - 快速查找命令
2. **TROUBLESHOOTING.md** - 查找解决方案
3. **RIP_TRACKING_GUIDE.md** - 使用跟踪工具诊断
4. **CURRENT_STATUS.md** - 了解已知问题

---

## 🎯 关键信息速查

### 当前状态
- ✅ 内核可以加载和启动
- ✅ 页表配置正确(higher-half mapping)
- ✅ 中断已启用(RFLAGS.IF)
- ✅ RIP跟踪系统已实现
- ⏳ 需要运行测试确定内核卡住位置

### 下一步行动
1. 在PowerShell中运行 `.\run_test_proper.ps1`
2. 分析输出中的STUCK消息
3. 根据GPA范围确定需要实现的功能
4. 实现相应的设备模拟或中断处理

### 关键文件位置
- RIP跟踪代码: `src/vmm/src/windows/whpx_vcpu.rs:1047-1088`
- APIC stub: `src/vmm/src/builder.rs:1050-1150`
- 页表配置: `src/vmm/src/windows/vstate.rs:300-400`
- 测试程序: `examples/test_kernel_boot.rs`

### 常用命令
```powershell
# 构建
cargo build --release --example test_kernel_boot

# 运行测试
.\run_test_proper.ps1

# 分析日志
Select-String -Path test.log -Pattern "STUCK|Exit #"
```

---

## 📊 文档统计

- 核心文档: 6个
- 工具脚本: 3个
- 总文档大小: ~50KB
- 最后更新: 2026-03-18

---

## 🔗 相关资源

### 代码仓库
- 主仓库: libkrun (Windows WHPX port)

### 技术参考
- Windows Hypervisor Platform API
- x86_64 higher-half kernel mapping
- APIC/IOAPIC/LAPIC specification
- Linux kernel boot protocol

---

## 💡 文档维护

### 添加新文档时
1. 在此索引中添加链接
2. 更新"文档统计"部分
3. 考虑更新"阅读顺序"建议

### 删除文档时
1. 从此索引中移除链接
2. 检查其他文档中的交叉引用
3. 更新"文档统计"部分

### 更新文档时
1. 更新"最后更新"时间戳
2. 如果是重大更新,在IMPROVEMENT_SUMMARY.md中记录

---

**提示**: 如果不确定从哪里开始,请阅读 **CURRENT_STATUS.md**
