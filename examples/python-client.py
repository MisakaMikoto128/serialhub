#!/usr/bin/env python3
# SerialHub 是原始字节管道 —— 串口的"值"的语义 (如 Modbus 寄存器) 由你的协议层负责。
#
# 最小 Python 接入示例: 连桥数据端点 /ws, 收打印 HEX, 每秒发一行 "ping"。
# 依赖: pip install websockets
#
# 三条铁律 (详见用户手册「程序接入」):
# 1. 原始字节帧: /ws 只收发二进制, 本例不解释任何"值";
# 2. WS 消息边界 != 串口帧边界: recv 到的块是"送达时刻"的字节, 组帧自己做;
# 3. 多客户端广播: 串口收到的字节广播给所有客户端, 发送经队列串行化。
# 真实数据网址看桥卡片/启动输出 (管理台端口 != 数据端口)。
import asyncio

import websockets

DATA_URL = "ws://127.0.0.1:8090/ws"  # 改成你的桥数据网址


async def main():
    async with websockets.connect(DATA_URL) as ws:
        print("已连接:", DATA_URL)

        async def rx():  # 下行: 串口收到的原始字节, 打 HEX
            async for data in ws:
                print("收", data.hex(" "))

        async def tx():  # 上行: 每秒一行 "ping" (UTF-8 原始字节)
            while True:
                await asyncio.sleep(1)
                await ws.send(b"ping\n")
                print("发 ping")

        await asyncio.gather(rx(), tx())


asyncio.run(main())
