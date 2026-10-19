"""Private stdio protocol. The host supplies credentials in memory, never argv/files."""
import json
import sys
import threading
import uuid
from tool_results import failure_of, legacy_write_result

_lock = threading.RLock()


class ToolFailure(RuntimeError):
    def __init__(self, error, result=None):
        super().__init__(error.get("message", "工具参数校验失败"))
        self.error = error
        self.result = result


# Compatibility for existing local extensions; failures now also cover references,
# permissions, conflicts and incomplete operations, not just argument syntax.
ToolArgumentError = ToolFailure

class HostOperationError(RuntimeError):
    """A user-safe domain failure produced by the local desktop host."""
    code = "host_operation"


def emit(event):
    with _lock:
        print(json.dumps(event, ensure_ascii=False), flush=True)


def call(method, params):
    # LangGraph may flush checkpoints on a worker thread. Serialize request/reply
    # pairs so a tool cannot consume another caller's response.
    with _lock:
        request_id = str(uuid.uuid4())
        emit({"rpc": request_id, "method": method, "params": params})
        line = sys.stdin.readline()
        if not line:
            raise RuntimeError("桌面宿主连接已关闭")
        response = json.loads(line)
        if response.get("rpc") != request_id:
            raise RuntimeError("桌面宿主响应顺序不一致")
        if response.get("error"):
            raise HostOperationError(response["error"])
        result = response.get("result")
        failure = failure_of(result, tool_name=params.get("name") if method == "tool" else None,
                             legacy_operation=method == "tool" and legacy_write_result(params.get("name", "")))
        if failure is not None:
            raise ToolFailure(failure, result)
        if method == 'tool' and not (isinstance(result,dict) and result.get('status')=='confirmation_required'):
            from contracts import RESULT_TYPES
            schema=RESULT_TYPES.get(params.get('name'))
            if schema:
                from pydantic import TypeAdapter
                TypeAdapter(schema).validate_python(result)
        return result
