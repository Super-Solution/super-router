import sys, os
sys.path.insert(0, os.environ.get("SR_PY_SRC") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "backends", "python", "src"))
import uvicorn
from super_router.contracts import Principal
from super_router.router import Router, MockProvider
from super_router.server import Key, StaticKeys, create_app
from super_router.stores.memory import MemoryStore
from super_router.providers import OpenAIProvider
store = MemoryStore()
providers = {"openai": OpenAIProvider("sk-fake", base_url=os.environ.get("UPSTREAM", "http://127.0.0.1:3499")), "mock": MockProvider("success")}
owner = Principal("ws", "prod", "c1", ("invoke","service:write","billing:write","billing:read","operations:read","routing:write"))
router = Router(store, providers, "bench-digest-key-0123456789abcdef-0123")
app = create_app(router, store, StaticKeys([Key("sr_" + "a"*64, owner)]))
uvicorn.run(app, host="127.0.0.1", port=3412, log_level="warning", http="h11")
