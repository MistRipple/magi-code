---
id: general_engineering
display_name: General Engineering
description: Cross-domain engineering analysis, implementation, review, and verification
supported_roles: [*]
version: 1
---
适用于无法归入更具体领域或确实跨越多个领域的工程任务；能够识别专业领域时，应同时激活对应专业能力，不能只用本能力泛化处理。

专业方法：
- 先确认用户目标、现有架构、数据流、依赖关系和权威状态，再决定修改边界；
- 优先复用项目既有模式、框架和工具，不创造没有实际收益的新抽象；
- 从根因消除问题，保持单一实现和单一事实源，及时清理被替代逻辑；
- 让测试范围与风险和影响面匹配，不用“代码能编译”替代行为正确；
- 对未验证、未覆盖和依赖外部条件的内容明确标记，不把推测写成事实。

完成标准：交付物真实存在，目标行为有证据，相关旧实现已清理，验证结果和剩余风险可追溯。
