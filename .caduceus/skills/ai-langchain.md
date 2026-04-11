---
name: ai-langchain
version: "1.0"
description: LangChain patterns — LCEL chains, RAG pipelines, tool-using agents, and streaming responses
categories: [ai, llm, integration]
triggers: ["langchain chain lcel", "langchain rag retrieval", "langchain tool agent", "langchain streaming", "llm agent python langchain"]
tools: [read_file, edit_file, shell, run_tests]
---

# LangChain Agent Patterns Skill

## Setup
```bash
pip install langchain langchain-openai langchain-community chromadb
```

## Basic LCEL Chain (LangChain Expression Language)
```python
from langchain_openai import ChatOpenAI
from langchain_core.prompts import ChatPromptTemplate
from langchain_core.output_parsers import StrOutputParser

llm = ChatOpenAI(model="gpt-4o-mini", temperature=0)

prompt = ChatPromptTemplate.from_messages([
    ("system", "You are a helpful assistant for {domain}."),
    ("human", "{question}"),
])

chain = prompt | llm | StrOutputParser()

# Sync
answer = chain.invoke({"domain": "software engineering", "question": "What is DRY?"})

# Async
answer = await chain.ainvoke({"domain": "software engineering", "question": "What is DRY?"})

# Streaming
async for chunk in chain.astream({"domain": "engineering", "question": "Explain SOLID"}):
    yield chunk
```

## RAG Pipeline (Retrieval-Augmented Generation)
```python
from langchain_openai import OpenAIEmbeddings
from langchain_community.vectorstores import Chroma
from langchain.text_splitter import RecursiveCharacterTextSplitter
from langchain_core.runnables import RunnablePassthrough

# 1. Chunk and embed documents
splitter = RecursiveCharacterTextSplitter(chunk_size=1000, chunk_overlap=200)
chunks = splitter.split_documents(raw_docs)
vectorstore = Chroma.from_documents(chunks, OpenAIEmbeddings(), persist_directory="./chroma")
retriever = vectorstore.as_retriever(search_kwargs={"k": 4})

# 2. Build RAG chain
rag_prompt = ChatPromptTemplate.from_template(
    "Answer based ONLY on this context:\n{context}\n\nQuestion: {question}"
)
rag_chain = (
    {"context": retriever, "question": RunnablePassthrough()}
    | rag_prompt
    | llm
    | StrOutputParser()
)

answer = await rag_chain.ainvoke("What is the refund policy?")
```

## Tool-Using Agent
```python
from langchain.agents import create_tool_calling_agent, AgentExecutor
from langchain_core.tools import tool

@tool
def search_orders(customer_id: str) -> str:
    "Search all orders for a given customer ID. Returns JSON list."
    orders = db_sync.get_orders(customer_id)
    return str([o.to_dict() for o in orders])

agent = create_tool_calling_agent(llm, tools=[search_orders], prompt=agent_prompt)
executor = AgentExecutor(
    agent=agent,
    tools=[search_orders],
    verbose=True,
    max_iterations=5,        # always set — prevents infinite loops
    handle_parsing_errors=True,
)
result = await executor.ainvoke({"input": "Find orders for customer C123"})
```

## Conversation Memory
```python
from langchain.memory import ConversationBufferWindowMemory
memory = ConversationBufferWindowMemory(k=5, return_messages=True)
```

## Best Practices
- Always set `max_iterations` on `AgentExecutor` — prevents runaway tool calls
- Track token usage and costs via LangSmith (`LANGCHAIN_TRACING_V2=true`)
- Cache LLM calls in development: `from langchain.cache import InMemoryCache; set_llm_cache(InMemoryCache())`
- Use `chain.with_structured_output(MyPydanticModel)` for typed, parseable responses
- Prefer async (`ainvoke`, `astream`) for all production FastAPI/web contexts
