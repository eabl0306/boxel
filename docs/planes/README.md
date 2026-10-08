# Planes de diseño

Planes técnicos completos (en inglés) que salieron de las rondas de diseño con varias propuestas y un
juez. Las decisiones que ya se tomaron están en los ADR (`docs/adr/`); acá queda el plan entero, con
las fases que todavía faltan, para no perder detalle.

| Plan | Estado (2026-10-08) |
|---|---|
| [caves-plan.md](caves-plan.md) | Fases 1-2 y 4 hechas (ruido 3D, `OverworldGenerator`, cave culling del ADR 0022: la puerta de la superficie no pasa). Faltan 3 (presupuesto de subida), 5 (radio 32: Hi-Z o subceldas de 16³, arena que crece), 6 (contenido en profundidad), 7 (API `Pipeline`). |
| [light-plan.md](light-plan.md) | P0-P6 hechas (luz de color, AO, día y noche, lámparas; ADR 0015). P7 medida (`docs/rendimiento/luz-fisicas-2026-10.md`). Falta P8 (sombras del sol, luces dinámicas, filtros de vidrio, etc.). |
| [physics-plan.md](physics-plan.md) | P0-P4 hechas (ADR 0016). Falta P5 (step-up, formas no cúbicas, fluidos, fricción, jinetes, esfera exacta, distancia de simulación, `cast_box`). |

Los pendientes de todos los planes están como issues en el GitHub Project del repo.
