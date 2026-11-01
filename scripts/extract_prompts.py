"""從 Python 原始碼用 ast 抽出 prompt 字串（f-string 的變數轉成 {name} 佔位符）。"""
import ast, sys, pathlib

SRC = pathlib.Path(sys.argv[1])
OUT = pathlib.Path(sys.argv[2])

def render(node):
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.JoinedStr):
        out = []
        for v in node.values:
            if isinstance(v, ast.Constant):
                out.append(v.value)
            else:
                out.append("{" + ast.unparse(v.value) + "}")
        return "".join(out)
    raise TypeError(ast.dump(node)[:200])

def tree(path):
    return ast.parse((SRC / path).read_text(encoding="utf-8"))

def module_assign(path, name):
    for n in tree(path).body:
        if isinstance(n, ast.Assign) and any(getattr(t, "id", None) == name for t in n.targets):
            return n.value
    raise KeyError(name)

def func_assign(path, func, var):
    for n in ast.walk(tree(path)):
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and n.name == func:
            for m in ast.walk(n):
                if isinstance(m, ast.Assign) and any(getattr(t, "id", None) == var for t in m.targets):
                    return m.value
    raise KeyError(f"{func}.{var}")

def func_return(path, func):
    for n in ast.walk(tree(path)):
        if isinstance(n, ast.FunctionDef) and n.name == func:
            for m in ast.walk(n):
                if isinstance(m, ast.Return) and m.value is not None and isinstance(m.value, (ast.JoinedStr, ast.Constant)):
                    return m.value
    raise KeyError(func)

def write(name, text):
    (OUT / f"{name}.txt").write_text(text, encoding="utf-8")
    print(f"{name}.txt: {len(text)} chars")

for k, v in zip(*[(lambda d: (d.keys, d.values))(module_assign("tools/summarizer.py", "SUMMARIZE_PROMPTS"))][0]):
    write(f"summarize_{render(k)}", render(v))
for k, v in zip(*[(lambda d: (d.keys, d.values))(module_assign("tools/youtube_tool.py", "YOUTUBE_PROMPTS"))][0]):
    write(f"youtube_{render(k)}", render(v))
write("humanize_guidelines", render(module_assign("loader/langtools.py", "HUMANIZE_GUIDELINES")))
write("social_media", render(func_return("loader/langtools.py", "_build_social_media_prompt")))
write("research_report", render(func_assign("loader/langtools.py", "generate_research_report", "prompt")))
write("bookmark_summary", render(func_assign("loader/langtools.py", "summarize_for_bookmark", "prompt")))
write("chat_instruction", render(module_assign("agents/chat_agent.py", "CHAT_AGENT_INSTRUCTION")))
write("vision_default", render(module_assign("agents/vision_agent.py", "DEFAULT_IMAGE_PROMPT")))
write("github_summary", render(func_assign("loader/gh_tools.py", "summarized_yesterday_github_issues", "prompt")))
write("restaurant_batch", render(func_assign("services/batch_service.py", "generate_restaurant_analysis_jsonl", "prompt")))
write("nearby_batch", render(func_assign("tools/maps_tool.py", "get_nearby_restaurants_for_batch", "query")))
write("specific_restaurant", render(func_assign("tools/maps_tool.py", "search_specific_restaurant_by_name", "query")))
write("restaurant_reviews", render(func_assign("tools/maps_tool.py", "get_nearby_restaurants_for_batch", "search_query")))
write("restaurant_names", render(func_assign("tools/maps_tool.py", "search_nearby_places", "extract_prompt")))
write("voice_system", render(func_return("services/voice_live.py", "build_system_instruction")))
write("help", render(func_assign("agents/orchestrator.py", "_handle_command", "help_text")))
