# Planes de diseño

Planes técnicos completos (en inglés) que salieron de las rondas de diseño con varias propuestas y un
juez. Las decisiones que ya se tomaron están en los ADR (`docs/adr/`); acá queda el plan entero, con
las fases que todavía faltan, para no perder detalle.

| Plan | Estado (2026-10-08) |
|---|---|
| [caves-plan.md](caves-plan.md) | Fases 1-2 y 4 hechas (ruido 3D, `OverworldGenerator`, cave culling del ADR 0022: la puerta de la superficie no pasa). Faltan 3 (presupuesto de subida), 5 (radio 32: Hi-Z o subceldas de 16³, arena que crece), 6 (contenido en profundidad), 7 (API `Pipeline`). |
| [light-plan.md](light-plan.md) | P0-P6 hechas (luz de color, AO, día y noche, lámparas; ADR 0015). P7 medida (`docs/rendimiento/luz-fisicas-2026-10.md`). Falta P8 (sombras del sol, luces dinámicas, filtros de vidrio, etc.). |
| [physics-plan.md](physics-plan.md) | P0-P4 hechas (ADR 0016). Falta P5 (step-up, formas no cúbicas, fluidos, fricción, jinetes, esfera exacta, distancia de simulación, `cast_box`). |
| [sky-sources-plan.md](sky-sources-plan.md) | Aprobado por el dueño (2026-11-02, #140, reemplaza a #130): puntos que emiten cielo en un haz de hasta 256 bloques, guardados como estado de bloque, y luz ambiente y sombreado de caras propios por reino. Va la opción A (canal de cielo, curva y sombreado por reino en el shader). Nada hecho todavía. |
| [colored-sky-plan.md](colored-sky-plan.md) | Aprobado (2026-11-02), respuestas del dueño en su sec. 11: el cielo que pasa por un vidrio de color se tiñe, como la luz de bloque del ADR 0060. Recomienda la opción D: tres canales de cielo solo donde el color se ve, la misma entrada de 8 bytes en la GPU y nada nuevo en un mundo sin vidrio de color. Nada hecho todavía. |
| [sun-shadows-plan.md](sun-shadows-plan.md) | Propuesta (2026-11-02, #9), espera las respuestas del dueño de su sec. 9. Recomienda la opción A: sombras en cascadas solo en High, dibujadas desde el arena con las caras que no miran al sol, que solo oscurecen la luz del cielo (si algo falla queda la imagen de hoy). Mid y Low no cambian. Nada hecho todavía. |

Los pendientes de todos los planes están como issues en el GitHub Project del repo.
