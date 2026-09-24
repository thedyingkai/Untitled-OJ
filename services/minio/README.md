# 旧 MinIO 元数据

此目录的 v1 Service/Release 模板保留给已有部署与历史导入，不作为新部署推荐。
不改写其历史版本、Service ID 或对象数据；旧模板里的镜像地址也不代表当前仍可拉取。

新自托管对象存储使用[固定摘要的 SeaweedFS 配方](../../deploy/object-store/README.md)，
Storage 通过标准 S3 接口连接。已有 MinIO 必须先确认对象、元数据和运维策略的迁移，
不能将旧 `minio-data` 卷直接挂到新 provider，也不能用新空卷自动替换旧实例。
