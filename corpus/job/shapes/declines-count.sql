-- declines: not a MIN or a MAX
-- COUNT(*) counts join rows, and a reduction keeps sets, not counts.
SELECT COUNT(*) AS n FROM title AS t, movie_keyword AS mk WHERE t.id = mk.movie_id;
