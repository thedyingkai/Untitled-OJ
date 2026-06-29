# Service 来源模型

Root Installer 支持本地目录、Service package、GitHub 仓库、普通 Git URL 和预构建 release artifact。

拉取远程来源时必须选择 branch、tag 或 commit，读取并校验 `service.yaml`，生成 install plan，再配置 Endpoint 和 Link。

安装流程不能执行仓库任意脚本、hook、任意 command，不信任 arbitrary image，不允许 privileged、cap_add 或 host mount。
