"""LangGraph checkpoint adapter to the existing platform record service.

Only serialization and storage adaptation live here; graph execution and replay
remain upstream responsibilities. Retain ancestor chains for DeltaChannel replay.

每次调用都带 thread_id：宿主按 (会话, 线程, 命名空间) 寻址，批量步骤等子线程
与主线程互不可见，也读不到其他会话的检查点。
"""
import base64
from langgraph.checkpoint.base import BaseCheckpointSaver, CheckpointTuple, WRITES_IDX_MAP
from host import call


class HostSaver(BaseCheckpointSaver):
    def pack(self, value):
        kind, data = self.serde.dumps_typed(value)
        return [kind, base64.b64encode(data).decode("ascii")]

    def unpack(self, value):
        return self.serde.loads_typed((value[0], base64.b64decode(value[1])))

    @staticmethod
    def address(config):
        c = config["configurable"]
        return c["thread_id"], c.get("checkpoint_ns", ""), c.get("checkpoint_id")

    def get_tuple(self, config):
        thread, namespace, checkpoint_id = self.address(config)
        record = call("checkpoint_get", {"thread": thread, "namespace": namespace, "id": checkpoint_id})
        if not record:
            return None
        actual = {"configurable": {"thread_id": thread, "checkpoint_ns": namespace, "checkpoint_id": record["id"]}}
        parent = {"configurable": {**actual["configurable"], "checkpoint_id": record["parent"]}} if record.get("parent") else None
        return CheckpointTuple(actual, self.unpack(record["checkpoint"]),
                               self.unpack(record["metadata"]), parent,
                               [(w["task"], w["channel"], self.unpack(w["value"])) for w in record.get("writes", [])])

    def put(self, config, checkpoint, metadata, new_versions):
        thread, namespace, parent = self.address(config)
        call("checkpoint_put", {"thread": thread, "namespace": namespace, "id": checkpoint["id"], "parent": parent,
                                "checkpoint": self.pack(checkpoint), "metadata": self.pack(metadata)})
        return {"configurable": {"thread_id": thread, "checkpoint_ns": namespace, "checkpoint_id": checkpoint["id"]}}

    def put_writes(self, config, writes, task_id, task_path=""):
        thread, namespace, checkpoint_id = self.address(config)
        call("checkpoint_writes", {"thread": thread, "namespace": namespace, "id": checkpoint_id,
                                   "writes": [{"task": task_id, "index": WRITES_IDX_MAP.get(channel, index),
                                               "channel": channel, "value": self.pack(value)}
                                              for index, (channel, value) in enumerate(writes)]})

    def list(self, config, *, filter=None, before=None, limit=None):
        thread, namespace, _ = self.address(config)
        ids = call("checkpoint_list", {"thread": thread, "namespace": namespace})
        before_id = self.address(before)[2] if before else None
        count = 0
        for checkpoint_id in ids:
            if before_id and checkpoint_id >= before_id:
                continue
            result = self.get_tuple({"configurable": {**config["configurable"], "checkpoint_id": checkpoint_id}})
            if filter and any(result.metadata.get(k) != v for k, v in filter.items()):
                continue
            yield result
            count += 1
            if limit is not None and count >= limit:
                break
